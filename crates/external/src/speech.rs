//! Read Out Loud through the system's speech command. The text goes to the program's standard
//! input, never into its command line, and is spoken as plain text (no markup):
//!
//! - Windows: Windows PowerShell running a fixed, encoded script that reads standard input and
//!   speaks it with System.Speech;
//! - macOS: `/usr/bin/say -f -`, with `[[` (its embedded-command marker) broken up;
//! - Linux and others: `espeak-ng --stdin` without markup, when installed. Speech Dispatcher's
//!   `spd-say` is not used: in pipe mode it reads lines starting `!-!` as commands.
//!
//! One utterance at a time; Stop (or starting another) ends the program and everything it started.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

use crate::{ExternalError, supervise};

/// The most text spoken in one go, in bytes.
pub const MAX_SPEECH: usize = 1 << 20;

/// The speech program in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechBackend {
    WindowsPowerShell(PathBuf),
    MacSay(PathBuf),
    EspeakNg(PathBuf),
}

/// The fixed PowerShell script: read UTF-8 text from standard input and speak it as plain text.
const PS_SCRIPT: &str = "$ErrorActionPreference='Stop'\n\
Add-Type -AssemblyName System.Speech\n\
$reader = New-Object System.IO.StreamReader([Console]::OpenStandardInput(), (New-Object System.Text.UTF8Encoding $false))\n\
$text = $reader.ReadToEnd()\n\
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer\n\
$s.SetOutputToDefaultAudioDevice()\n\
$s.Speak($text)\n\
$s.Dispose()\n";

/// Base64 (RFC 4648) of `bytes`.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                // The index is six bits, so always within the table.
                out.push(char::from(T[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// PowerShell's `-EncodedCommand` form of [`PS_SCRIPT`]: base64 of its UTF-16LE bytes.
fn encoded_script() -> String {
    let utf16: Vec<u8> = PS_SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&utf16)
}

impl SpeechBackend {
    /// The speech program this system has, if any.
    pub fn detect() -> Option<SpeechBackend> {
        if cfg!(windows) {
            let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let ps = system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
            return ps.is_file().then_some(SpeechBackend::WindowsPowerShell(ps));
        }
        if cfg!(target_os = "macos") {
            let say = PathBuf::from("/usr/bin/say");
            return say.is_file().then_some(SpeechBackend::MacSay(say));
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join("espeak-ng")).find(|p| p.is_file()).map(SpeechBackend::EspeakNg)
    }

    /// The program's name, for messages.
    pub fn name(&self) -> &'static str {
        match self {
            SpeechBackend::WindowsPowerShell(_) => "Windows speech",
            SpeechBackend::MacSay(_) => "macOS speech",
            SpeechBackend::EspeakNg(_) => "eSpeak NG",
        }
    }

    /// The command (fixed arguments only) and the bytes to send on its standard input.
    pub(crate) fn command(&self, text: &str) -> (Command, Vec<u8>) {
        match self {
            SpeechBackend::WindowsPowerShell(ps) => {
                let mut c = Command::new(ps);
                c.args(["-NoProfile", "-NonInteractive", "-NoLogo", "-EncodedCommand"]).arg(encoded_script());
                (c, text.as_bytes().to_vec())
            }
            SpeechBackend::MacSay(say) => {
                let mut c = Command::new(say);
                c.args(["-f", "-"]);
                // `say` reads `[[...]]` as embedded commands; break the marker up.
                (c, text.replace("[[", "[ [").into_bytes())
            }
            SpeechBackend::EspeakNg(e) => {
                let mut c = Command::new(e);
                c.arg("--stdin");
                (c, text.as_bytes().to_vec())
            }
        }
    }
}

/// The speech in progress: at most one program at a time.
#[derive(Default)]
pub struct Speech {
    child: Option<Child>,
    writer: Option<JoinHandle<()>>,
}

impl Speech {
    /// Speak `text` with `backend`, stopping anything already being spoken. Text past
    /// [`MAX_SPEECH`] bytes is cut at a character boundary.
    pub fn start(&mut self, backend: &SpeechBackend, text: &str) -> Result<(), ExternalError> {
        self.stop();
        let mut cut = text.len().min(MAX_SPEECH);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let text = text.get(..cut).unwrap_or_default();
        if text.trim().is_empty() {
            return Err(ExternalError::InvalidInput("there is no text to read".into()));
        }
        let (mut cmd, input) = backend.command(text);
        cmd.stdin(Stdio::piped());
        let mut child = supervise::spawn(cmd, backend.name())?;
        let stdin = child.stdin.take();
        // Writing on a thread: a program that stops reading can't block the caller. Closing the
        // pipe afterwards tells programs that read to the end that the text is complete.
        self.writer = stdin.map(|mut pipe| {
            std::thread::spawn(move || {
                let _ = pipe.write_all(&input);
            })
        });
        self.child = Some(child);
        Ok(())
    }

    /// Stop speaking: end the program and what it started. Never waits on the text writer: a
    /// helper that kept the input pipe open without reading could block it, and Stop must not
    /// hang the caller. The writer ends when the pipe's readers are gone.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => supervise::end_leftovers(&child),
                _ => supervise::end_tree(&mut child),
            }
        }
        self.writer = None;
    }

    /// Whether something is being spoken now.
    pub fn speaking(&mut self) -> bool {
        match self.child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => true,
            Some(_) => {
                self.stop();
                false
            }
            None => false,
        }
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    /// Text that looks like commands to PowerShell, a shell or Speech Dispatcher stays on standard
    /// input and never reaches the command line.
    #[test]
    fn text_never_reaches_the_command_line() {
        let text = "$(Remove-Item C:\\x) `whoami` ; rm -rf / !-!SET SELF RATE 100 [[rate 500]] \u{0}";
        for b in [
            SpeechBackend::WindowsPowerShell(PathBuf::from("powershell.exe")),
            SpeechBackend::MacSay(PathBuf::from("/usr/bin/say")),
            SpeechBackend::EspeakNg(PathBuf::from("/usr/bin/espeak-ng")),
        ] {
            let (cmd, input) = b.command(text);
            let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert!(args.iter().all(|a| !a.contains("whoami") && !a.contains("!-!") && !a.contains("rate")), "{b:?}: {args:?}");
            assert!(String::from_utf8_lossy(&input).contains("whoami"), "{b:?}: the text goes to stdin");
            if matches!(b, SpeechBackend::MacSay(_)) {
                assert!(!String::from_utf8_lossy(&input).contains("[["), "say's command marker is broken up");
            }
        }
    }

    /// Final-review finding F4: a program that exits at once while a helper holds its input pipe
    /// without reading must not make Stop (or dropping the speech) hang.
    #[cfg(unix)]
    #[test]
    fn stop_does_not_hang_on_a_helper_holding_the_input() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("espeak-ng");
        std::fs::write(&fake, "#!/bin/sh\nsleep 60 <&0 &\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = SpeechBackend::EspeakNg(fake);
        let mut s = Speech::default();
        s.start(&backend, &"word ".repeat(200_000)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let started = std::time::Instant::now();
        s.stop();
        drop(s);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "stop took {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn stop_ends_a_program_that_never_finishes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("espeak-ng");
        // Reads its input, then keeps "speaking".
        std::fs::write(&fake, "#!/bin/sh\ncat > /dev/null\nsleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = SpeechBackend::EspeakNg(fake);
        let mut s = Speech::default();
        s.start(&backend, "hello").unwrap();
        assert!(s.speaking());
        let started = std::time::Instant::now();
        s.stop();
        assert!(!s.speaking() && started.elapsed() < std::time::Duration::from_secs(5));
        assert!(matches!(s.start(&backend, "   "), Err(ExternalError::InvalidInput(_))));
        // Starting again replaces the first utterance.
        s.start(&backend, "one").unwrap();
        s.start(&backend, "two").unwrap();
        assert!(s.speaking());
    }
}
