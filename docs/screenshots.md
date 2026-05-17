# Screenshots

Use reproducible screenshots instead of ad hoc desktop captures. The website
repo owns the visual assets under `shots/*.png` so the app repo does not
duplicate large binaries.

## Website Captures

From the sibling `agentnoise.org` repo:

```sh
python3 -m http.server 4177
scripts/site-shot http://127.0.0.1:4177 shots/desktop.png 1800 1440
scripts/site-shot http://127.0.0.1:4177 shots/mobile.png 390 900
scripts/site-shot http://127.0.0.1:4177 shots/mobile-long.png 390 1900
```

If the global `site-shot` service is preferred:

```sh
site-shotd start
site-shot -w 1800 -h 1440 --retina http://127.0.0.1:4177 shots/desktop.png
site-shot -w 390 -h 900 --retina http://127.0.0.1:4177 shots/mobile.png
```

## Open Graph Image

The social image is generated from the SVG source:

```sh
magick -background none assets/og-agentnoise.svg assets/og-agentnoise.png
```

## README Images

Prefer site-hosted images in Markdown so screenshots stay in one place:

```md
![agentnoise desktop](https://agentnoise.org/shots/desktop.png)
![agentnoise mobile](https://agentnoise.org/shots/mobile.png)
```

Commit and push the refreshed `shots/*.png` files in `agentnoise.org` before
linking to them from the app README.

## Privacy Checklist

- Do not capture a real `nsec`, full phone `npub`, private relay URL, private
  repo path, or personal chat history.
- Use a burner identity or staged text for pairing and command examples.
- Truncate public keys in screenshots unless the exact public identity is part
  of the thing being documented.
- Prefer `/status`, `/help`, and a tiny `/codex Reply exactly: done` example
  over real work logs.
