'use strict';

const SCOPE = '@konnect-xai';
const PLATFORM_KEYS = Object.freeze([
  'linux-x64',
  'linux-arm64',
  'darwin-x64',
  'darwin-arm64',
  'win32-x64',
]);

class UnsupportedPlatformError extends Error {}
class MissingBinaryError extends Error {}

function platformKey(platform, arch) {
  const key = `${platform}-${arch}`;
  return PLATFORM_KEYS.includes(key) ? key : null;
}

const packageFor = (key) => `${SCOPE}/konnect-${key}`;
const binaryFile = (platform) => (platform === 'win32' ? 'konnect.exe' : 'konnect');

function resolveBinary({ platform, arch, resolve }) {
  const key = platformKey(platform, arch);
  if (key === null) {
    throw new UnsupportedPlatformError(
      `konnect has no prebuilt binary for ${platform}-${arch}. Supported platforms: ${PLATFORM_KEYS.join(', ')}.`,
    );
  }
  const id = `${packageFor(key)}/bin/${binaryFile(platform)}`;
  try {
    return resolve(id);
  } catch (e) {
    if (e && e.code === 'MODULE_NOT_FOUND') {
      throw new MissingBinaryError(
        `The platform package ${packageFor(key)} is not installed. ` +
          'It is an optional dependency, so it is skipped when npm runs with --no-optional or --omit=optional. ' +
          'Reinstall without that flag.',
      );
    }
    throw e;
  }
}

module.exports = {
  PLATFORM_KEYS,
  platformKey,
  packageFor,
  binaryFile,
  resolveBinary,
  UnsupportedPlatformError,
  MissingBinaryError,
};
