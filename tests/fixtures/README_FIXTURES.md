# Offline Test Fixtures (`tests/fixtures/`)

All files in `tests/fixtures/` are **100% synthetic, deterministic, offline, and strictly read-only**. Automated tests MUST NEVER read `\\192.168.0.12\...` and MUST copy fixtures into a per-test `tempfile::TempDir` before performing any rename, move, or delete operations.

---

## 1. Adobe Illustrator XML Fixtures (`tests/fixtures/*.xml`)

Universal byte invariants across all 7 XML fixtures:
- **Encoding**: UTF-8 **without BOM** (first 4 bytes `3C 3F 78 6D` = `<?xm`).
- **Line Endings**: **100% CRLF (`\r\n`, `0x0D 0x0A`)**, including a single trailing `\r\n` after `</svg>`. Zero bare `\n` or bare `\r`.
- **DTD Prolog**: Standard Adobe Illustrator `<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 20001102//EN"    "http://www.w3.org/TR/2000/CR-SVG-20001102/DTD/svg-20001102.dtd" [...]>` declaring all 6 `&ns_*;` entities (`ns_graphs`, `ns_vars`, `ns_imrep`, `ns_custom`, `ns_flows`, `ns_extend`).

| File | Variant / Purpose | Indent | `varSetName` | `<variable>` Attr Order | Samples | Key Edge Cases Covered |
|---|---|---|---|---|---|---|
| `template_text.xml` | Standard Text-only template | 4 spaces | `ModeloTexto` | `category, trait, varName` | 0 (empty) | Empty `<v:sampleDataSets>`, 3 `Text` vars (`NomeCompleto`, `Cargo`, `CodigoRegistro`) |
| `template_image.xml` | Standard Image-only template | 4 spaces | `ModeloImagem` | `category, trait, varName` | 1 | `LinkedFile` var (`Foto`, `trait="fileref"`), raw path without `<p>` |
| `template_mixed.xml` | Clean standard Text + Image template | 4 spaces | `ModeloMisto` | `category, trait, varName` | 1 | `Nome`, `Funcao`, `Matricula` (`00001`), `Foto`; zero-warning happy-path |
| `template_visibility_graph.xml` | Out-of-scope `W07` preservation fixture | 4 spaces | `ModeloComExtras` | `category, trait, varName` | 1 | `Text` + `LinkedFile` + `Visibility` (`SeloVisivel`) + `GraphData` (`GraficoDesempenho`) |
| `secom_reference.xml` | Mirrors `crachás SECOM.xml` | 4 spaces | `CrachasCorporativos` | `category, trait, varName` | 3 | Blank line before `<v:sampleDataSets>`, zero-padded `dataSetName`, pt-BR accents, raw Windows paths |
| `segunda_leva_variant.xml` | Mirrors `Segunda leva.xml` | 4 spaces | `CrachasCorporativos` | `category, trait, varName` | 2 | `<svg xmlns:c2pa="..."><metadata><c2pa:manifest>...</c2pa:manifest></metadata>`, double-space attributes, double-spaced nested filename |
| `bombeiro_reference.xml` | Mirrors `bombeiro.xml` | 2 spaces | `binding1` | `varName, trait, category` | 3 | `varSetName` before `locked`, legacy `trait="textcontent"` on `<foto>`, empty `<p></p>`, expanded literal URLs on `<v:sampleDataSets>`, CRLF-indented `file:///.../(1).png` |

---

## 2. CSV Fixtures (`tests/fixtures/*.csv`)

All CSV fixtures use `\r\n` (CRLF) record terminators.

| File | Encoding | BOM | Delimiter | Header Row (0-based) | Data Rows | Key Edge Cases Covered |
|---|---|---|---|---|---|---|
| `sample_utf8_comma.csv` | UTF-8 | No | `,` | `0` (`codigo,nome,funcao,observacoes,foto`) | 6 | Unquoted & quoted leading zeros (`00421`, `"00007"`, `00000`), quoted comma, quoted embedded `\n` (8 raw lines across 6 records), escaped quotes `""Geral""`, empty cells, duplicate `codigo`/`nome`/`foto`, multiple spaces |
| `sample_bombeiros_semicolon.csv` | UTF-8 | No | `;` | `0` (`matricula;nome;funcao;foto`) | 6 | `;` delimiter, empty `funcao`, `file:///D:/Fotos/bombeiro/(9).png`, duplicate `funcao` & `matricula`, `.tiff` ref |
| `sample_bombeiros_header_row2.csv` | UTF-8 | No | `;` | `1` (`matricula;nome;funcao;foto`) | 4 | Metadata title banner on row 1 (0-based `0`), actual headers on row 2 (0-based `1`), duplicate `matricula`, empty cells |
| `sample_win1252_semicolon.csv` | Windows-1252 (`cp1252`) | No | `;` | `0` (`matricula;nome;funcao;observacao`) | 3 | Genuine `cp1252` invalid under UTF-8 (`0xE3` `ã` at byte 42, `0x96` `–` en-dash, `0xE7` `ç`, `0xE9` `é`, `0xC1` `Á`), leading zeros, empty `funcao` |
| `sample_utf8_bom_tab.csv` | UTF-8 | **Yes** (`EF BB BF`) | `\t` | `0` (`codigo\tnome\tsetor\tfoto`) | 4 | UTF-8 BOM stripping (first header must be `"codigo"`, not `"\u{FEFF}codigo"`), tab delimiter, double space + accents, duplicate `setor`/`codigo`, empty `setor` |

---

## 3. XLSX Fixture (`tests/fixtures/sample_badges.xlsx`)

Standard 8-part OpenXML archive (`[Content_Types].xml`, `_rels/.rels`, `xl/workbook.xml`, `xl/_rels/workbook.xml.rels`, `xl/styles.xml`, `xl/sharedStrings.xml`, `xl/worksheets/sheet1.xml`, `xl/worksheets/sheet2.xml`).

| Sheet | Index | Header Row (0-based) | Columns | Data Rows | Key Edge Cases Covered |
|---|---|---|---|---|---|
| `Crachas` | 0 | `0` | `codigo`, `nome`, `funcao`, `numero_raw`, `foto` | 5 | Leading zeros via `t="s"` (`"00421"`, `"00000"`) and `t="inlineStr"` (`"00007"`), numeric int (`421`, `0`), `.0` float (`1000.0`), decimal floats (`12.5`, `3.14159`), omitted empty cells (`C4`, `E6`), explicit empty tag (`<c r="E4"/>`), `xml:space="preserve"` (`"Marcela  Gonçalves"`), duplicates |
| `ListaExtra` | 1 | `1` | `matricula`, `servidor`, `cargo`, `pontuacao` | 3 | Row 1 title banner (`RELATÓRIO EXTRA - CRACHÁS 2026`), actual headers on row 2, leading zeros (`"00888"`, `"00001"`), Portuguese accents, empty cell (`C4`), duplicate `matricula` |

---

## 4. Synthetic Image & Media Fixtures (`tests/fixtures/images/`)

| File Path | Format / Template | Size (B) | Magic Bytes | Purpose |
|---|---|---|---|---|
| `joao_silva.jpg` | `MINIMAL_JPEG_2X2` (2×2 Baseline-DCT) | 683 | `FF D8 FF E0` | Standard `.jpg` |
| `maria_conceicao.jpeg` | `MINIMAL_JPEG_2X2` | 683 | `FF D8 FF E0` | 4-char `.jpeg` extension |
| `carlos_eduardo.png` | `MINIMAL_PNG_2X2` (2×2 8-bit RGBA) | 134 | `89 50 4E 47` | Standard `.png` |
| `aguia_dourada.tiff` | `MINIMAL_TIFF_2X2` (2×2 uncompressed RGB) | 152 | `49 49 2A 00` | Uncompressed baseline `.tiff` |
| `UPPERCASE_EXT.PNG` | `MINIMAL_PNG_2X2` | 134 | `89 50 4E 47` | Case-insensitive extension `.PNG` |
| `Ana Silva.jpg` | `MINIMAL_JPEG_2X2` | 683 | `FF D8 FF E0` | Same-stem ambiguity tie pair (#1) |
| `Ana Silva.png` | `MINIMAL_PNG_2X2` | 134 | `89 50 4E 47` | Same-stem ambiguity tie pair (#2) |
| `MARCELA  GONÇALVES.jpg` | `MINIMAL_JPEG_2X2` | 683 | `FF D8 FF E0` | Uppercase, accent (`Ç`), double space |
| `subpasta/nested_foto.tif` | `MINIMAL_TIFF_2X2` | 152 | `49 49 2A 00` | Recursive `walkdir` scan + `.tif` extension |
| `ignored_sample.heic` | Synthetic dummy HEIC header | 23 | `66 74 79 70` | Unsupported `.heic` ignored by scanner |
| `ignored_document.pdf` | Synthetic dummy PDF header | 16 | `25 50 44 46` | Unsupported `.pdf` ignored by scanner |

---

## 5. Offline Regeneration Recipe

All fixtures are generated 100% offline using **Python 3.15.1 stdlib-only** (`pathlib`, `zipfile`, `xml.sax.saxutils`) — **no `openpyxl`, no `Pillow`, no network shares (`\\192.168.0.12\...`)**:

1. Write XML & UTF-8 CSV files via `"\r\n".join(lines) + "\r\n"` encoded as UTF-8 (`utf-8`, no BOM), with `sample_utf8_bom_tab.csv` prefixed by `b"\xEF\xBB\xBF"` and `sample_win1252_semicolon.csv` encoded with `.encode("cp1252")`.
2. Build `sample_badges.xlsx` via `zipfile.ZipFile(..., "w", compression=zipfile.ZIP_DEFLATED)` writing the 8 OpenXML XML parts (`[Content_Types].xml`, `_rels/.rels`, `xl/workbook.xml`, `xl/_rels/workbook.xml.rels`, `xl/styles.xml`, `xl/sharedStrings.xml`, `xl/worksheets/sheet1.xml`, `xl/worksheets/sheet2.xml`).
3. Write the 2×2 JPEG (683 B), PNG (134 B), TIFF (152 B), and dummy `.heic`/`.pdf` byte arrays directly to `tests/fixtures/images/`.
4. Verify byte-level BOM/CRLF & `.NET XmlReader` (`DtdProcessing::Parse`, `XmlResolver = $null`), Python `cp1252`/`utf-8` CSV checks, Python `zipfile`/`ElementTree` XLSX checks, and `System.Drawing.Image::FromStream` 2×2 image decoding.
