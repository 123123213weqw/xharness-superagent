export default {
 root:new URL('.',import.meta.url).pathname,
 resolve:{alias:[
  {find:/^vitest$/,replacement:new URL('./upstream/node_modules/vitest/dist/index.js',import.meta.url).pathname},
  {find:/^@deepseek-ai\/(.+)$/,replacement:new URL('./upstream/node_modules/@deepseek-ai/',import.meta.url).pathname+'$1'},
 ]},
 test:{include:['live-skills-differential.spec.ts'],maxWorkers:1,fileParallelism:false,testTimeout:60000},
}
