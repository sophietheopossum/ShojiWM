import type {
  EnvController,
  EnvUpdatePayload,
  EnvValue,
} from "./types";

interface PendingEnvOperation {
  key: string;
  value?: string;
}

const pendingOperations: PendingEnvOperation[] = [];
const pendingPublishKeys = new Set<string>();
const desiredEnvironment = new Map<string, string>();

function normalizeKey(key: string): string {
  const normalized = String(key);
  if (!normalized || normalized.includes("=") || normalized.includes("\0")) {
    throw new Error(`invalid environment variable name: ${JSON.stringify(key)}`);
  }
  return normalized;
}

function normalizeValue(value: EnvValue): string {
  if (value == null) {
    throw new Error("environment variable value must not be null or undefined");
  }
  return String(value);
}

function processEnv(): Record<string, string | undefined> {
  return (
    (globalThis as { process?: { env?: Record<string, string | undefined> } })
      .process?.env ?? {}
  );
}

export const ENV_CONTROLLER: EnvController = {
  set(key, value) {
    const normalizedKey = normalizeKey(key);
    const normalizedValue = normalizeValue(value);
    processEnv()[normalizedKey] = normalizedValue;
    pendingOperations.push({
      key: normalizedKey,
      value: normalizedValue,
    });
    desiredEnvironment.set(normalizedKey, normalizedValue);
  },
  unset(key) {
    const normalizedKey = normalizeKey(key);
    delete processEnv()[normalizedKey];
    pendingOperations.push({ key: normalizedKey });
    desiredEnvironment.delete(normalizedKey);
  },
  get(key) {
    return processEnv()[normalizeKey(key)];
  },
  apply(values) {
    for (const [key, value] of Object.entries(values)) {
      if (value == null) {
        this.unset(key);
      } else {
        this.set(key, value);
      }
    }
  },
  publish(keys) {
    const targetKeys = keys ?? desiredEnvironment.keys();
    for (const key of targetKeys) {
      pendingPublishKeys.add(normalizeKey(key));
    }
  },
};

export function drainPendingEnvUpdates(): EnvUpdatePayload | undefined {
  if (pendingOperations.length === 0 && pendingPublishKeys.size === 0) {
    return undefined;
  }

  const payload: EnvUpdatePayload = {
    operations: pendingOperations.splice(0, pendingOperations.length),
    publish: Array.from(pendingPublishKeys).sort(),
  };
  pendingPublishKeys.clear();
  return payload;
}
