# dopedb site

Next.js landing page for the open-source dopedb desktop app.

Production URL: https://dopedb-cjs1301.vercel.app

## Local development

```sh
pnpm --dir site install
pnpm site:preview-image
pnpm site:dev
```

## Vercel

The site is hosted on the `cjs1301` Vercel account. To redeploy from this repo:

```sh
vercel deploy site --yes --prod
```

If importing the repository through the Vercel dashboard, set **Root Directory** to
`site`. The default install/build commands are enough:

- Install: `pnpm install`
- Build: `pnpm build`
- Output: Next.js default

Downloads point to the repository's latest GitHub Release.
