// Rollback target for the first web promotion. Serve no app content or assets.
export default {
  fetch: () => new Response('NNTune is temporarily unavailable.\n', {
    status: 503,
    headers: {
      'cache-control': 'no-store',
      'content-type': 'text/plain; charset=utf-8',
      'x-robots-tag': 'noindex',
    },
  }),
};
