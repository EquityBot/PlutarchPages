# pdfcraft-external

Layer L4, native only (the engine leaves it out of the web build). Optional programs the user
installed separately, used by PlutarchPages for the PDF-XChange features that need them:

- `office_to_pdf`: Office files (doc, docx, odt, rtf, xls, xlsx, ods, ppt, pptx, odp; no
  macro-enabled formats) through LibreOffice's `soffice --headless --convert-to pdf`. LibreOffice
  older than 26.2.5 is refused (`MIN_LIBREOFFICE`): earlier releases have document-parser
  memory-safety bugs fixed in 26.2.5 (for example CVE-2026-4430 and CVE-2026-8357).
- `html_to_pdf`: a local HTML file through Chrome, Edge or Chromium (`--headless=new
  --print-to-pdf`), with scripts off and every network request refused (unknown host names and a
  dead proxy, loopback included).
- `url_to_pdf`: a public web page (`checked_web_url`: http or https, no user name or password,
  nothing on this computer or a private network in any numeric spelling), scripts and network on.
- `Speech`: Read Out Loud through the system's speech program, text on standard input only:
  Windows PowerShell with a fixed encoded System.Speech script, macOS `say -f -` (with `[[` broken
  up), or `espeak-ng --stdin` on Linux. Speech Dispatcher's `spd-say` is not used (its pipe mode
  reads lines starting `!-!` as commands).

Each conversion runs in a private `tempfile` folder: a fixed input name (`in/input.<ext>`), a fresh
browser or LibreOffice profile, home, cache and temporary folders inside it, absolute paths only,
and the target after `--` so it can't be read as a switch. The program runs in its own process
group with a deadline (120 s) and a cancel flag; an early stop ends the whole tree (`killpg` through
rustix on Unix, `taskkill /T /F` on Windows). The result must be a regular file under 512 MB that
starts like a PDF and ends with `%%EOF`; the engine then parses it before showing it.

This is not an operating-system sandbox. A converter that a hostile file compromises runs with the
user's rights, and a local HTML file can still include other local files it names (as a browser
would). The engine therefore keeps conversion off until the user turns it on (Preferences ▸ External
converters, or `PDFCRAFT_EXTERNAL_CONVERTERS=1` for the CLI and MCP server); the programs are
located from `PDFCRAFT_CHROME` / `PDFCRAFT_SOFFICE`, the usual install folders, then `PATH`.

The programs are never linked, copied, bundled or downloaded (AGENTS.md §2).
