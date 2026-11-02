import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { runInNewContext } from "node:vm";

test("the real worklet acknowledges only after its final audio and reports the probed native frame", () => {
  const messages: Array<Record<string,unknown>>=[];
  type Processor={port:{onmessage:(e:{data:Record<string,unknown>})=>void};process:(inputs:Float32Array[][])=>boolean};
  let constructor: new (options:unknown)=>Processor;
  const scope={currentFrame:1000,AudioWorkletProcessor:class {port={postMessage:(message:Record<string,unknown>)=>messages.push(message),onmessage:()=>undefined};},registerProcessor:(_name:string,value:typeof constructor)=>{constructor=value;}};
  runInNewContext(readFileSync(resolve("public/pcm-capture-worklet.js"),"utf8"),scope);
  const processor=new constructor!({processorOptions:{channels:1}});
  processor.port.onmessage({data:{type:"clockProbe",probeId:"probe"}});
  processor.process([[new Float32Array(128).fill(0.1)]]);
  expect(messages[0]).toMatchObject({type:"clockSample",probeId:"probe",frame:1000,firstFrame:1000});
  scope.currentFrame=1128;
  processor.process([[new Float32Array(128).fill(0.2)]]);
  processor.port.onmessage({data:{type:"stop"}});
  expect(messages.map((m)=>m.type)).toEqual(["clockSample","audio","stopped"]);
  expect(messages[1]).toMatchObject({frame:1000,frames:256});
  expect(messages[2]).toMatchObject({firstFrame:1000,finalFrame:1256});
  expect(processor.process([[new Float32Array(128)]])).toBe(false);
});
