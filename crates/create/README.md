# pdfcraft-create

Layer L4. Create a PDF (Acrobat's Create a PDF tool), execution plan M10.2:

- `blank(width, height, pages)`: an empty document;
- `from_images(&[(name, bytes)])`: one page per image, sized from the image's resolution
  (PNG `pHYs`, JPEG JFIF density; 72 dpi when absent). JPEG data is embedded as is
  (`/DCTDecode`, grey, RGB or Adobe-inverted CMYK); PNG is decoded and stored with Flate,
  with transparency as a soft mask. An ICC profile in the file (JPEG `APP2 ICC_PROFILE`, PNG
  `iCCP`) that matches the image's colour space is kept as an `/ICCBased` colour space with
  the device space as `/Alternate`; images with the same profile share one profile object.
  JPEG metadata scanning skips marker fill bytes and reads application segments after the
  frame header, stopping at the first scan or end marker. Broken or mismatched ICC profiles
  still fall back to the device colour space;
- `from_images_with_resolution(images, ImageResolution::Dpi(dpi))`: override the page
  resolution (1–1200 dpi), without resampling or recompressing image pixels. Use 72 dpi
  for one point per pixel, or `ImageResolution::Embedded` for the same behavior as
  `from_images`. The largest page side is still capped at 14,400 points;
- `from_text(text, …)`: plain text set in Helvetica, wrapped and paginated. Text Helvetica
  can't draw is refused, naming the line and the character, never set as `?`.
- `from_text_escaped(text, …)`: the same for reports PdfCraft generates from documents
  (comment summaries, compare reports): such characters are written as `[U+XXXX]` and the first
  line says so (`ESCAPED_NOTE`).
- `from_markdown(title, text, page)`: CommonMark (tables, strikethrough, task lists) through
  pulldown-cmark, laid out by `flow`: headings, emphasis, inline and block code, quotes, nested
  lists, rules, tables and link annotations (http, https and mailto only). Images are never
  fetched; their alt text shows. Raw HTML is left out;
- `from_csv(title, name, text, page)`: an RFC 4180 CSV or TSV file (delimiter sniffed among
  `,` `;` tab `|`) as one table whose header row repeats on every page; more than 6 columns turn
  the page and shrink the type. At most 100,000 rows and 64 columns; `CsvReport` says what was cut;
- `from_email(title, bytes, page, embed_attachments)`: an `.eml` message through mail-parser:
  subject, addresses, date and every text body (an HTML-only body becomes plain text here, with
  nothing fetched). Attachments are listed, and embedded as file attachments only when asked.

`flow` sets text in the standard 14 fonts with WinAnsiEncoding, so a character outside it prints
as `?`; every flow-based function reports how many, so callers can say so.

Every function returns a `pdfcraft_cos::Document`; the caller writes it.

Image export and Save Image As apply each lossless image's Decode pair before converting its color components.
Indexed Decode values select palette entries after rounding and clipping. Color-key masks compare the original samples.
Missing, null or malformed Decode arrays keep the default mapping.
Soft-mask Decode intervals map samples to opacity before the mask is resized, including reversed ranges and indirect entries.
The existing 16-bit path still uses each sample's high byte.
JPEG image Decode handling remains limited.
ICC transforms and calibrated color conversion remain outside this export path.
