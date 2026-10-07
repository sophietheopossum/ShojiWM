// The slice of `node:test` and `node:assert/strict` these tests use, so they
// typecheck without `@types/node`. That package cannot be added: its global
// `process: NodeJS.Process` collides with the embedded runtime's `process` in
// `packages/shoji_wm/src/runtime-globals.d.ts`, which every test imports via
// `shoji_wm`'s index. Signatures follow `@types/node`; extend them as tests
// start using more of either module.

declare module "node:test" {
  type TestFn = () => void | Promise<void>;
  export function test(name: string, fn: TestFn): Promise<void>;
  export function afterEach(fn: TestFn): void;
}

declare module "node:assert/strict" {
  type AssertPredicate =
    | RegExp
    | (new (...args: any[]) => object)
    | ((thrown: unknown) => boolean)
    | object
    | Error;

  interface StrictAssert {
    equal<T>(actual: unknown, expected: T, message?: string | Error): asserts actual is T;
    deepEqual<T>(actual: unknown, expected: T, message?: string | Error): asserts actual is T;
    throws(block: () => unknown, error?: AssertPredicate, message?: string | Error): void;
    doesNotThrow(block: () => unknown, message?: string | Error): void;
    rejects(
      block: Promise<unknown> | (() => Promise<unknown>),
      error?: AssertPredicate,
      message?: string | Error,
    ): Promise<void>;
  }

  const assert: StrictAssert;
  export default assert;
}
