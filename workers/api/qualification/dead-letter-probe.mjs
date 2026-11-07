// Harness-only observer. A successful/acknowledged source delivery cannot arrive here.
export default {
  async queue(batch, env) {
    const pointers = batch.messages.map(message => message.body);
    await env.INGEST_SCRATCH.put('qualification-refused', JSON.stringify(pointers));
    batch.ackAll();
  },
};
