import {
  AdocWeaveClient,
  AdocWeaveClientError,
  AdocWeaveResult,
  PROTOCOL_SCHEMA_VERSION,
  analyzeOnce,
  defaultAssetUrls,
  isAdocWeaveClientLifecycleError,
} from "../index.mjs";

const client = new AdocWeaveClient({
  ...defaultAssetUrls(),
  onResult(result: AdocWeaveResult) {
    const html: string = result.html;
    const version: number = result.sourceVersion;
    const formulaSource: string | undefined =
      result.projection?.formulas[0]?.source;
    console.log(html, version, formulaSource);
  },
});
await client.ready;
try {
  const result: AdocWeaveResult = await client.analyze({
    version: 2,
    source: "= Promise",
  });
  console.log(result.html);
} catch (error) {
  if (isAdocWeaveClientLifecycleError(error)) console.error(error.code);
}
const once = await analyzeOnce(defaultAssetUrls(), { version: 3, source: "= Once" });
console.log(once.html, AdocWeaveClientError);
const protocolSchemaVersion: number = PROTOCOL_SCHEMA_VERSION;
console.log(protocolSchemaVersion);
client.update({
  version: 1,
  source: "= Typed",
  analysisOptions: {
    syntax: {
      limits: { maxInputBytes: 1024 * 1024 },
    },
  },
  renderPolicy: {
    activeUrls: { allowResolvedRootRelative: true },
    externalLinks: { openInNewContext: true, noreferrer: true },
    sourceLanguages: { allowed: ["rust"], unknown: "diagnostic" },
    mathLanguages: ["latex"],
    unresolvedReferences: "label-only",
    resources: { images: false, media: false },
    documentMode: "complete",
    stylesheets: [
      { kind: "inline", css: "p { margin: 0; }" },
      { kind: "external", url: "https://example.com/theme.css" },
    ],
  },
});
client.cancel();
client.dispose();
