import { AdocWeaveClient, BROWSER_PACKAGE_VERSION, defaultAssetUrls } from "./index.mjs";

const source = document.querySelector("#source");
const preview = document.querySelector("#preview");
const status = document.querySelector("#status");
let version = 0;
const client = new AdocWeaveClient({
  ...defaultAssetUrls(new URL("../worker/index.mjs", import.meta.url)),
  onResult(result) {
    preview.textContent = result.html;
    status.value = `ready:${result.sourceVersion}:${result.generation}`;
    globalThis.adocweaveLastResult = result;
  },
  onError(error) { status.value = `error:${error.code}`; },
});
globalThis.adocweavePackageVersion = BROWSER_PACKAGE_VERSION;
const update = () => client.update({ version: ++version, source: source.value });
source.addEventListener("input", update);
update();
client.update({ version: ++version, source: "= stale first\n" });
client.update({ version: ++version, source: "= stale second\n" });
client.cancel();
source.value = "= Latest browser result\n";
update();
