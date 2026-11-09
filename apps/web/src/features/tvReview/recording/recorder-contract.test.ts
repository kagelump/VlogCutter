import type { BrowserProjectClient, ReviewTargetDetail } from "@rcdvlog/project-client";
import { PROBE_RETRY_MS, VoiceRecording } from "./recorder";
import { MemoryOutbox } from "./outbox";

type Port={onmessage:((event:MessageEvent)=>void)|null;postMessage:ReturnType<typeof vi.fn>};
let port: Port;
class Context {
  sampleRate=48000;
  baseLatency=0;
  currentTime=0;
  state="running";
  destination={};
  onstatechange:(()=>void)|null=null;
  addEventListener() {}
  audioWorklet={addModule:vi.fn().mockResolvedValue(undefined)};
  createMediaStreamSource() { return {connect:(node:unknown)=>node}; }
  createGain() { return {gain:{value:0},connect:(node:unknown)=>node}; }
  async close() {this.state="closed";}
  async resume() {this.state="running";}
}
class Node {
  port:Port={onmessage:null,postMessage:vi.fn()};
  constructor(){port=this.port;}
  connect(node:unknown){return node;}
}
function message(data:unknown){port.onmessage?.({data} as MessageEvent);}
async function start() {
  vi.useFakeTimers({toFake:["setTimeout","clearTimeout","setInterval","clearInterval"]});
  vi.stubGlobal("AudioContext",Context);
  vi.stubGlobal("AudioWorkletNode",Node);
  const client={
    createReviewSession:vi.fn().mockResolvedValue({sessionId:"s"}),
    putReviewSessionChunk:vi.fn(async (_p:string,_s:string,chunk:{sequence:number;sha256:string})=>({...chunk,receivedAt:"t"})),
    recordReviewSessionEvent:vi.fn(async (_p:string,_s:string,event:Record<string,unknown>)=>({...event,serverReceivedAt:"t"})),
    finalizeReviewSession:vi.fn().mockResolvedValue({sessionId:"s",state:"completed"}),
    getReviewSession:vi.fn(),
  };
  const track={getSettings:()=>({channelCount:1}),addEventListener:vi.fn(),stop:vi.fn()};
  const stream={getAudioTracks:()=>[track],getTracks:()=>[track]} as unknown as MediaStream;
  const outbox=new MemoryOutbox();
  const recording=await VoiceRecording.start({client:client as unknown as BrowserProjectClient,outbox,projectId:"p",target:{reviewTargetId:"t"} as ReviewTargetDetail,stream,additionalSpeakersDisclosed:true,onStatus:()=>undefined,onEnded:()=>undefined});
  message({type:"audio",frame:1000,channels:[new Float32Array(512).fill(0.1)]});
  return {client,outbox,recording};
}
afterEach(()=>{vi.clearAllTimers();vi.useRealTimers();vi.unstubAllGlobals();});

test("stop waits beyond 150 ms for the final worklet batch and a concurrent stop shares its result", async()=>{
  const {client,recording}=await start();
  const stopped=recording.stop("stopped",1000,1000);
  expect(recording.stop()).toBe(stopped);
  await vi.advanceTimersByTimeAsync(200);
  expect(client.finalizeReviewSession).not.toHaveBeenCalled();
  message({type:"audio",frame:1512,channels:[new Float32Array(128).fill(0.2)]});
  message({type:"stopped",firstFrame:1000,finalFrame:1640});
  await stopped;
  expect(client.finalizeReviewSession).toHaveBeenCalledWith("p","s",{finalFrameCount:640,endReason:"stopped"});
  expect(client.putReviewSessionChunk.mock.calls[0][2]).toMatchObject({frameCount:640,startFrame:0});
});

test("a probe answered too late for an honest bracket is retried soon, not a clock interval later",async()=>{
  const {client}=await start();
  const probes=()=>port.postMessage.mock.calls.map(([m])=>m as {type:string;probeId:string}).filter((m)=>m.type==="clockProbe");
  expect(probes().length).toBeGreaterThan(0);
  const now=vi.spyOn(performance,"now");
  const base=performance.now();
  now.mockReturnValue(base+900); // beyond the 500 ms bracket bound
  message({type:"clockSample",probeId:probes()[0].probeId,frame:1000,firstFrame:1000});
  now.mockReturnValue(base+1_000);
  const before=probes().length;
  await vi.advanceTimersByTimeAsync(PROBE_RETRY_MS+1);
  expect(probes().length).toBe(before+1);
  message({type:"clockSample",probeId:probes().at(-1)!.probeId,frame:1100,firstFrame:1000});
  await vi.advanceTimersByTimeAsync(10);
  now.mockRestore();
  expect(client.recordReviewSessionEvent).toHaveBeenCalledWith("p","s",expect.objectContaining({kind:"clockSample",detail:expect.objectContaining({method:"workletProbeBracket"})}));
});

test("unacknowledged stop records unknown tail evidence instead of a clean stop",async()=>{
  const {client,recording}=await start();
  const stopped=recording.stop("stopped",1000,250);
  await vi.advanceTimersByTimeAsync(251);
  await stopped;
  expect(client.recordReviewSessionEvent).toHaveBeenCalledWith("p","s",expect.objectContaining({kind:"pageInterrupted",detail:expect.objectContaining({captureTailStatus:"unknown"})}));
  expect(client.finalizeReviewSession).toHaveBeenCalledWith("p","s",expect.objectContaining({endReason:"interrupted"}));
});

test("player observations are persisted with playback and capture clocks before finalization", async()=>{
  const {client,recording}=await start();
  recording.observePlayback(23.5, 1, 1, 1234);
  const stopped=recording.stop("stopped",1000,1000);
  await vi.advanceTimersByTimeAsync(10);
  message({type:"stopped",firstFrame:1000,finalFrame:1512});
  await stopped;
  expect(client.recordReviewSessionEvent).toHaveBeenCalledWith("p","s",expect.objectContaining({
    kind:"clockSample", performanceMs:expect.any(Number),
    detail:expect.objectContaining({method:"playerObservation",clockEpoch:0,playback:expect.objectContaining({positionSeconds:23.5,state:1,rate:1,observedPerformanceMs:1234})}),
  }));
});
