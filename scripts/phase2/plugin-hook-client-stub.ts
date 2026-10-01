export class DaemonClient {
  async connect() {}
  async close() {}
}

export function createPaseoApi() {
  return { dispose: async () => undefined };
}
