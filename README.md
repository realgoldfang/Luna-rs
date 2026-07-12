# luna-nspire

A Rust port of [Luna](https://github.com/ndless-nspire/Luna) v2.1 — a portable command-line converter of Lua and Python scripts to TI-Nspire `.tns` documents. Produces **byte-identical** output to the original C binary.

Lua scripts require OS 3.0.2 or later. Python scripts require CX II OS 5.2 or later.
It can also convert TI-Nspire XML problems/documents and pack arbitrary files (e.g. images) into TNS documents.

## Installation

```sh
cargo install luna-nspire
```

Or build from source:

```sh
cargo build --release
```

Requires the `zlib` development library (`zlib1g-dev` on Debian/Ubuntu, `zlib-devel` on Fedora, `brew install zlib` on macOS).

## Usage

```sh
# Lua program conversion
luna-nspire INFILE.lua OUTFILE.tns

# Problem conversion
luna-nspire Problem1.xml OUTFILE.tns

# Multiple files
luna-nspire Document.xml Problem1.xml [Problem2.xml...] OUTFILE.tns

# Python conversion
luna-nspire InFile1.py [InFile2.py...] OUTFILE.tns

# Read Lua from stdin
luna-nspire - OUTFILE.tns
```

- If the input is `-`, Lua is read from standard input.
- Files should be UTF-8 encoded if they contain special characters.
- For Python, the first script will be the one shown when the TNS document is opened.
- BMP files automatically set the TI-Nspire version header to 0x0700.

## Supported Input Types

| Extension | Description |
|-----------|-------------|
| `.lua`    | Lua scripts (wrapped in TI XML with CDATA) |
| `.py`     | Python scripts (wrapped in TI Python XML) |
| `.xml`    | TI-Nspire problem/document XML (compressed + encrypted) |
| `.bmp`    | Bitmap resources (stored, sets version 0x0700) |
| Other     | Arbitrary files (deflate-compressed into TNS) |

## License

Licensed under the [Mozilla Public License v1.1](LICENSE).

DES implementation based on code by Eric Young (eay@cryptsoft.com).
Based on a derived version of MiniZip — see the original [minizip-1.1/MiniZip64_info.txt](https://github.com/ndless-nspire/Luna/blob/master/minizip-1.1/MiniZip64_info.txt) for details.
