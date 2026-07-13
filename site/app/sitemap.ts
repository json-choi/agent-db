import type { MetadataRoute } from "next";

const siteUrl = "https://dopedb.dev";

export default function sitemap(): MetadataRoute.Sitemap {
  return [
    {
      url: siteUrl,
      changeFrequency: "weekly",
      priority: 1,
      alternates: {
        languages: {
          en: siteUrl,
          ko: `${siteUrl}/ko`,
        },
      },
    },
    {
      url: `${siteUrl}/ko`,
      changeFrequency: "weekly",
      priority: 1,
      alternates: {
        languages: {
          en: siteUrl,
          ko: `${siteUrl}/ko`,
        },
      },
    },
    {
      url: `${siteUrl}/llms.txt`,
      changeFrequency: "monthly",
      priority: 0.8,
    },
  ];
}
