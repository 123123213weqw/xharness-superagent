export default {
  root: new URL('.', import.meta.url).pathname,
  resolve: { dedupe: ['react', 'react-dom'] },
  test: { include: ['capture.spec.ts'], maxWorkers: 1, fileParallelism: false },
};
