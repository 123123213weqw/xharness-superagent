export default {
  root: new URL('.', import.meta.url).pathname,
  resolve:{dedupe:['react','react-dom'],alias:[{find:/^vitest$/,replacement:new URL('./upstream/node_modules/vitest/dist/index.js',import.meta.url).pathname}]},
  test:{include:['migration-differential.spec.ts'],maxWorkers:1,fileParallelism:false,testTimeout:60000},
}
