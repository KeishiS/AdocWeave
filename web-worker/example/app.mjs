import { AdocWeaveClient, BROWSER_PACKAGE_VERSION, defaultAssetUrls } from "../worker/index.mjs";

const source = document.querySelector("#source");
const preview = document.querySelector("#preview");
const status = document.querySelector("#status");
let version = 0;

const client = new AdocWeaveClient({
  // Explicit base keeps asset URLs stable when this file is bundled.
  ...defaultAssetUrls(new URL("../worker/index.mjs", import.meta.url)),
  onResult(result) {
    // textContent keeps the example independent of a host HTML trust policy.
    preview.textContent = result.html;
    status.value = `ready:${result.sourceVersion}:${result.generation}`;
    globalThis.adocweaveLastResult = result;
  },
  onError(error) {
    status.value = `error:${error.code}`;
  },
});

function update() {
  client.update({ version: ++version, source: source.value });
}
source.addEventListener("input", update);
update();
globalThis.adocweaveExample = { client, source, preview, status };
globalThis.adocweavePackageVersion = BROWSER_PACKAGE_VERSION;

if (new URL(location.href).searchParams.has("smoke")) {
  client.update({ version: ++version, source: "= stale first\n" });
  client.update({ version: ++version, source: "= stale second\n" });
  client.cancel();
  source.value = "= Latest browser result\n";
  update();
  await new Promise((resolve, reject) => {
    const deadline = Date.now() + 15000;
    const wait = () => {
      if (status.value.startsWith("ready:") || status.value.startsWith("error:")) resolve();
      else if (Date.now() >= deadline) reject(new Error(`browser smoke timeout: ${status.value}`));
      else setTimeout(wait, 25);
    };
    wait();
  });
}
