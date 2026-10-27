# superproxy.dev

The home page, served by Super itself on Railway, and the docs it proxies from docs.page.

- `public/`: the page (`index.html`, one file: its styles and script inline), its share image
  (`og.png`), icons, font (Outfit, OFL, Latin subset), `robots.txt` and `sitemap.xml`
- `config.hcl`: `superproxy.dev` from `public/`, `www.` redirected to it, and
  `docs.superproxy.dev` proxied to docs.page (Cloudflare's headers removed, which docs.page's own
  Cloudflare refuses)
- `Dockerfile`: Super's image, with both

Railway builds this folder (its root directory: `site`) and routes both names to it, on port
8080: its edge, and Cloudflare in front of it, handle HTTPS.

To see it locally: `super -c` a copy of `config.hcl` with `/var/www/site` replaced by
`site/public`, then `curl -H 'Host: superproxy.dev' localhost:8080`.
