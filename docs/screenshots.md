# Screenshots

Use reproducible product screenshots instead of ad hoc desktop captures. Do not
use marketing-site viewport screenshots in the docs; they look like page crops
and do not explain the app.

The sibling `agentnoise.org` repo owns a dedicated `shots.html` staging page
and the generated `shots/*.png` assets so the app repo does not duplicate large
binaries.

## Product Captures

From the sibling `agentnoise.org` repo:

```sh
python3 -m http.server 4177
scripts/site-shot http://127.0.0.1:4177/shots.html shots/desktop.png 1600 1200
scripts/site-shot http://127.0.0.1:4177/shots.html shots/mobile.png 390 900
scripts/site-shot http://127.0.0.1:4177/shots.html shots/mobile-long.png 390 1200
```

If the global `site-shot` service is preferred:

```sh
site-shotd start
site-shot -w 1600 -h 1200 --retina http://127.0.0.1:4177/shots.html shots/desktop.png
site-shot -w 390 -h 900 --retina http://127.0.0.1:4177/shots.html shots/mobile.png
```

## Open Graph Image

The social image is generated from the SVG source:

```sh
magick -background none assets/og-agentnoise.svg assets/og-agentnoise.png
```

## README Images

If a README needs an image, use only the product-stage captures:

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
