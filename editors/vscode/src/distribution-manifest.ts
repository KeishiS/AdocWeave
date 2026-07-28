import type { ManagedPlatform } from "./platform.js";

export interface DistributionAsset {
  readonly archive: "zip";
  readonly byteSize: number;
  readonly executable: string;
  readonly kind: "lsp";
  readonly name: string;
  readonly sha256: string;
  readonly target: string;
}

export interface DistributionManifest {
  readonly assets: readonly unknown[];
  readonly packageVersion: string;
  readonly schemaVersion: 2;
  readonly sourceCommit: string;
}

const manifestKeys = ["assets", "packageVersion", "schemaVersion", "sourceCommit"];
const assetKeys = ["archive", "byteSize", "executable", "kind", "name", "sha256", "target"];

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  return JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`invalid-manifest:${label}`);
  }
  return value as Record<string, unknown>;
}

export function parseDistributionManifest(
  text: string,
  expectedVersion: string,
): DistributionManifest {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error("invalid-manifest:json");
  }
  const root = object(parsed, "root");
  if (!exactKeys(root, manifestKeys)) throw new Error("invalid-manifest:fields");
  if (
    root.schemaVersion !== 2 ||
    root.packageVersion !== expectedVersion ||
    typeof root.sourceCommit !== "string" ||
    !/^[0-9a-f]{40}$/.test(root.sourceCommit) ||
    !Array.isArray(root.assets)
  ) {
    throw new Error("invalid-manifest:identity");
  }
  return root as unknown as DistributionManifest;
}

export function selectLspAsset(
  manifest: DistributionManifest,
  platform: ManagedPlatform,
): DistributionAsset {
  const expectedName = `adocweave-lsp-${platform.target}.${platform.archive}`;
  const matches = manifest.assets.filter((entry) => {
    const candidate = object(entry, "asset");
    return candidate.kind === "lsp" && candidate.target === platform.target;
  });
  if (matches.length !== 1) throw new Error("invalid-manifest:lsp-asset-count");
  const asset = object(matches[0], "asset");
  if (!exactKeys(asset, assetKeys)) throw new Error("invalid-manifest:asset-fields");
  if (
    asset.archive !== platform.archive ||
    asset.executable !== platform.executable ||
    asset.name !== expectedName ||
    !Number.isSafeInteger(asset.byteSize) ||
    (asset.byteSize as number) < 1 ||
    typeof asset.sha256 !== "string" ||
    !/^[0-9a-f]{64}$/.test(asset.sha256)
  ) {
    throw new Error("invalid-manifest:lsp-asset");
  }
  return asset as unknown as DistributionAsset;
}
