# screeny-studio

Screeny Studio: design generative pieces for the panel and preview them as LEDs.
Today a Tauri v2 desktop app over the `screeny-art` pipeline (`crates/art`); the
plan in [`docs/design/studio-vision.md`](../../docs/design/studio-vision.md) turns it
into a server-first web app (card 105) that runs unattended in Docker and decides
what streams to the panel.

```bash
cargo run -p screeny-studio      # not part of the workspace's default members
```

Pieces, the pipeline, keys and the studio's meters are documented in
[`crates/art/README.md`](../art/README.md).
