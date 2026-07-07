import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/** @type {import('next').NextConfig} */
const nextConfig = {
  turbopack: {
    root: __dirname,
  },
  async redirects() {
    return [
      {
        source: "/:path*",
        has: [{ type: "host", value: "dopedb-cjs1301.vercel.app" }],
        destination: "https://dopedb.dev/:path*",
        permanent: true,
      },
    ];
  },
};

export default nextConfig;
