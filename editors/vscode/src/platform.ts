import { release as operatingSystemRelease } from "node:os";

import platformData from "../resources/platforms.json";

export interface ManagedPlatform {
  readonly architecture: string;
  readonly archive: "zip";
  readonly executable: string;
  readonly minimumOsVersion: string | null;
  readonly os: NodeJS.Platform;
  readonly target: string;
}

const platforms = platformData.supported as readonly ManagedPlatform[];

export function platformForHost(
  os: NodeJS.Platform = process.platform,
  architecture: string = process.arch,
  hostRelease: string = operatingSystemRelease(),
): ManagedPlatform {
  const matches = platforms.filter(
    (candidate) => candidate.os === os && candidate.architecture === architecture,
  );
  const [match] = matches;
  if (matches.length !== 1 || !match) {
    throw new Error(`unsupported-platform:${os}:${architecture}`);
  }
  if (!supportsOperatingSystemRelease(match, hostRelease)) {
    throw new Error(`unsupported-os-version:${os}:${hostRelease}`);
  }
  return match;
}

export function supportedPlatforms(): readonly ManagedPlatform[] {
  return platforms;
}

function supportsOperatingSystemRelease(platform: ManagedPlatform, hostRelease: string): boolean {
  if (!platform.minimumOsVersion) return true;
  const actual = hostRelease.split(".").map(Number);
  const required = platform.minimumOsVersion.split(".").map(Number);
  if (actual.some((part) => !Number.isSafeInteger(part))) return false;
  if (platform.os === "darwin") {
    const minimumMacMajor = required[0];
    return minimumMacMajor !== undefined && (actual[0] ?? 0) >= minimumMacMajor + 9;
  }
  for (let index = 0; index < Math.max(actual.length, required.length); index += 1) {
    const difference = (actual[index] ?? 0) - (required[index] ?? 0);
    if (difference !== 0) return difference > 0;
  }
  return true;
}
