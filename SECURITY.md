# Security policy

PlutarchPages opens files from strangers, so a crash, a data leak or a redaction that leaves content
behind counts as a security problem.

## Reporting

Report a vulnerability privately through
[GitHub's private reporting](https://github.com/EquityBot/PlutarchPages/security/advisories/new).
Please don't open a public issue for it. Include the version or commit, your platform and a
synthetic file that reproduces the problem. Don't attach a document with personal data.

If the problem also affects upstream [PdfCraft](https://github.com/storytold/pdfcraft), say so; it
may need reporting there as well. Upstream's team doesn't handle reports about this fork.

## Supported versions

Only the latest commit on `main` and the latest release, once there is one, receive fixes.

## Limits you can rely on

- Redaction removes marked content from the file, and refuses, changing nothing, when content under
  the marks is also used where the marks don't reach. Check the result before you share it.
- Password protection uses AES-256 unless you choose an older compatibility level. The older
  levels (40-bit and 128-bit RC4, and 128-bit AES) derive their keys with MD5 and exist only so
  older readers can open the file. RC4 and MD5 are broken, so choose one only when a reader needs it.
- Optional converters run programs you installed yourself (LibreOffice, Chrome or Edge, eSpeak NG)
  as separate processes. They are off by default where they convert files.
- The MCP server and the UI control channel run only when you start them.
