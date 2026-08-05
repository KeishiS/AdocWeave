import releaseManifest from "../release-manifest.json" with { type: "json" };

export const RELEASE_NOTES_VERSION = releaseManifest.packageVersion;
export const PUBLIC_PROTOCOL_SCHEMA_VERSION = 10;
