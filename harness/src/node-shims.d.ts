declare module "node:child_process" {
  export function execFile(
    file: string,
    args: string[],
    options: { timeout?: number; windowsHide?: boolean },
    callback: (error: unknown, stdout: string, stderr: string) => void,
  ): void;
}

declare module "node:util" {
  export function promisify<T extends (...args: any[]) => any>(fn: T): (...args: any[]) => Promise<any>;
}

declare module "node:fs/promises" {
  export function readFile(path: string, encoding: string): Promise<string>;
}

declare module "node:test" {
  export default function test(name: string, fn: () => void | Promise<void>): void;
}

declare module "node:assert/strict" {
  const assert: {
    equal(actual: unknown, expected: unknown): void;
    match(value: string, regex: RegExp): void;
  };
  export default assert;
}

declare const process: {
  argv: string[];
  stdout: { write(data: string): void };
  stderr: { write(data: string): void };
  exitCode: number;
};
