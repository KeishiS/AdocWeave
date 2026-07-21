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
    console.log(html, version);
  },
});
client.update({ version: 1, source: "= Typed" });
client.cancel();
client.dispose();
