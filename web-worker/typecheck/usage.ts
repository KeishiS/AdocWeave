import {
  AdocWeaveClient,
  AdocWeaveResult,
  defaultAssetUrls,
} from "../index.mjs";

const client = new AdocWeaveClient({
  ...defaultAssetUrls(),
  onResult(result: AdocWeaveResult) {
    const html: string = result.html;
    const version: number = result.sourceVersion;
    const formulaSource: string | undefined =
      result.result.projection.formulas[0]?.source;
    console.log(html, version, formulaSource);
  },
});
client.update({
  version: 1,
  source: "= Typed",
  options: {
    urlPolicy: { allowResolvedRootRelative: true },
    externalLinks: { openInNewContext: true, noreferrer: true },
    sourceLanguages: { allowed: ["rust"], unknown: "diagnostic" },
    mathLanguages: ["latex"],
    unresolvedReferences: "label-only",
    resources: { images: false, media: false },
  },
});
client.cancel();
client.dispose();
