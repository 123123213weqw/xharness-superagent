// Product acknowledgement/note word limits, not global language conformance.
import {readFile,writeFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
import {wordCount,checkedImText} from '../reference/upstream/src/infra/text.ts';
const samples=['修复颜色并补充测试。😀','hello, 中文 test 123!','[说明](https://example.com/a-b?q=x)','修复 '.repeat(100),'修复 '.repeat(101),'word '.repeat(100),'word '.repeat(101),'修复 '.repeat(30),'修复 '.repeat(31),'👩‍💻 😀 …'];
const cases=samples.map(text=>{let accepted=true;try{checkedImText(text)}catch{accepted=false}return{text,words:wordCount(text),im_accepted:accepted};});
const output={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'Normal business word limits only; ICU dictionary releases are not claimed globally identical',cases};
const file=new URL('./word-limits-oracle.json',import.meta.url);
if(process.argv.includes('--check'))assert.deepEqual(JSON.parse(await readFile(file,'utf8')),output);else await writeFile(file,JSON.stringify(output,null,2)+'\n');
console.log(`${cases.length} original business word-limit samples verified.`);
