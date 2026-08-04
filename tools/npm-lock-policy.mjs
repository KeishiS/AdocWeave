import { Buffer } from "node:buffer";

export function validSha512Integrity(integrity) {
  if (typeof integrity !== "string" || !integrity.startsWith("sha512-")) return false;
  const encoded = integrity.slice("sha512-".length);
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(encoded) || encoded.length % 4 !== 0) return false;

  const digest = Buffer.from(encoded, "base64");
  return digest.byteLength === 64 && digest.toString("base64") === encoded;
}

export function fetchedSafely(entry) {
  return (
    typeof entry.version === "string" &&
    validSha512Integrity(entry.integrity) &&
    typeof entry.resolved === "string" &&
    entry.resolved.startsWith("https://registry.npmjs.org/")
  );
}
