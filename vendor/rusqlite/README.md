# BoxDB SQLite wrapper

This is `boxdb-rusqlite`, a fork of rusqlite 0.40.2 used internally by
[BoxDB](https://github.com/lebedev-nikita/boxdb). It depends directly on
`boxdb-sqlite-sys`, which supplies BoxDB’s patched SQLite engine. Applications
should depend on `boxdb` rather than this implementation crate.

The Rust library retains the name `rusqlite`. Upstream source and its MIT
[license](LICENSE) are preserved; the manifest selects BoxDB’s engine and enables
the bundled build by default. System SQLite and SQLCipher are unsupported.

The package metadata records the upstream version used by BoxDB’s stock SQLite
benchmark. Update it when replacing the vendored upstream source.
