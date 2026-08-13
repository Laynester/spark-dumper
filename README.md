# sparkd

Export Director/Shockwave casts (`.cct`, `.dcr`, `.cst`) into playable,
editable project folders — bitmaps, sounds, scripts, texts, palettes, fonts,
shapes — that can be recompiled back into casts.

## Install

Grab a prebuilt binary from the [latest release](https://github.com/Laynester/spark-dumper/releases/latest) for your OS (macOS, Windows, Linux; x86_64 + aarch64) and put it on your PATH, or use one of the one-liners below.

macOS / Linux:

```sh
curl -sSL https://raw.githubusercontent.com/Laynester/spark-dumper/main/install.sh | sh
```

Windows (PowerShell):

```powershell
iwr https://raw.githubusercontent.com/Laynester/spark-dumper/main/install.ps1 -OutFile "$env:TEMP\sparkd-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\sparkd-install.ps1"
```

## Usage

```sh
sparkd <input file or directory> <output-directory> [-j N]
```

- `input` — a single `.cct` / `.dcr` / `.cst` file, or a directory tree (walked recursively).
- `output-directory` — where the export goes.
- `-j N` — worker threads for directory exports (default 1).

Examples:

```sh
# export one cast file
sparkd my_project.cct ./exported

# export a whole tree of casts, 18 threads
sparkd ./shockwave_dir ./exported -j 18
```

Each input cast exports a LibreShockwave-style project folder:

```
exported/
└── my_project/
    ├── bitmaps/    PNGs + .pal palette sidecars
    ├── sounds/     audio files
    ├── scripts/    decompiled Lingo source (.ls)
    ├── texts/
    ├── palettes/
    ├── fonts/      real .ttf
    ├── shapes/
    ├── movie.txt
    └── casts.txt
```

## Build from source

```sh
cargo build --release -p director-dump --bin sparkd
# binary: target/release/sparkd
```

## Debug / inspection flags

For a single input file, extra flags dump the internal structure:

```sh
sparkd file.cct ./out -x           # raw chunk hex dump
sparkd file.cct ./out --palettes   # palette resolution per bitmap
sparkd file.cct ./out -o rifx.bin  # write decompressed RIFX
```

## License

AGPL-3.0 — see [LICENSE](LICENSE). Some format-parsing and export behavior
derives from prior GPL/AGPL projects (e.g. ScummVM, DirPlayer,
LibreShockwave); attribution is retained in the source files.