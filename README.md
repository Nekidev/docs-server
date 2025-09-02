A minimal live-reload HTTP server for rustdoc.

To install it, run

```sh
$ cargo install docs-server
```

Running it is as easy as running the `docs` command at your crate's directory.

```sh
$ docs
02/09/2025 at 01:17:57.92 [INFO]  Getting cargo metadata...
02/09/2025 at 01:17:58.19 [INFO]  Compiling documentation for `docs-server`...
02/09/2025 at 01:17:58.74 [INFO]  Starting documentation server on address 0.0.0.0:8000...
02/09/2025 at 01:17:58.74 [INFO]  Documentation server is running on http://localhost:8000
...
```

You can now try to create, edit, or delete any file. You'll see displayed in your terminal:

```sh
02/09/2025 at 01:21:13.44 [INFO]  Source files changed, recompiling...
```

Reloading the web server in your browser will show the updated documentation.

To see the full list of options, run `docs --help`.
