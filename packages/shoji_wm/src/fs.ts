/**
 * Filesystem access for configs.
 *
 * The compositor embeds RustyScript with only the `web` feature set, so a
 * config has neither `node:fs` nor Deno's own fs extension: `node:` specifiers
 * do not resolve at all, and while `deno_fs` is compiled in as a dependency of
 * `web`, its ops are never registered. The compositor exposes a narrow read op
 * instead, in the same style as `__SHOJI_PATH_EXISTS__`.
 *
 * The `Deno.readTextFileSync` branch below is a fallback for builds that do
 * enable RustyScript's `fs` feature; it is not the normal path.
 */

interface DenoFsRuntime {
  readTextFileSync?(path: string): string;
}

/**
 * Read a UTF-8 text file.
 *
 * Throws if the file is missing, unreadable, or not valid UTF-8 — callers that
 * treat a missing file as "fall back to defaults" should catch.
 */
export function readTextFile(path: string): string {
  const nativeRead = (
    globalThis as typeof globalThis & {
      __SHOJI_READ_TEXT_FILE__?: (path: string) => string;
    }
  ).__SHOJI_READ_TEXT_FILE__;
  if (nativeRead) {
    return nativeRead(path);
  }

  const denoRuntime = (
    globalThis as typeof globalThis & { Deno?: DenoFsRuntime }
  ).Deno;
  if (denoRuntime?.readTextFileSync) {
    return denoRuntime.readTextFileSync(path);
  }

  throw new Error(
    `ShojiWM: cannot read ${path} — this runtime exposes no filesystem access`,
  );
}
