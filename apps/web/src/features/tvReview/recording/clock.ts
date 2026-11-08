// Real request/response brackets for an audio-thread sample-clock observation.
// Packet arrival alone is never assigned to recording sample zero.
export interface CaptureCalibration {
  schema: "review-capture-calibration.v1";
  method: "workletProbeBracket";
  clockId: string;
  sampleRate: number;
  firstSample: number;
  sampleIndex: number;
  monotonicNs: number;
  monotonicOriginNs: number;
  monotonicToleranceNs: number;
  probeSentNs: number;
  probeReceivedNs: number;
  performanceEpochMs: number;
  probeSentPerformanceMs: number;
  probeReceivedPerformanceMs: number;
}
export function captureCalibration(input: {
  clockId: string; sampleRate: number; firstSample: number;
  absoluteFrame: number; originFrame: number;
  epochMs: number; sentMs: number; receivedMs: number;
}): CaptureCalibration | null {
  const { clockId,sampleRate,firstSample,absoluteFrame,originFrame,epochMs,sentMs,receivedMs } = input;
  if (![sampleRate,firstSample,absoluteFrame,originFrame].every(Number.isSafeInteger)
    || sampleRate<8000 || sampleRate>192000 || firstSample<0 || originFrame<0 || absoluteFrame<originFrame
    || ![epochMs,sentMs,receivedMs].every(Number.isFinite) || epochMs<0 || sentMs<epochMs || receivedMs<sentMs || receivedMs-sentMs>500) return null;
  const sampleIndex=absoluteFrame-originFrame;
  if (sampleIndex<firstSample) return null;
  const monotonicNs=Math.round(((sentMs+receivedMs)/2-epochMs)*1e6);
  const monotonicOriginNs=monotonicNs-Math.round(sampleIndex/sampleRate*1e9);
  // Include one rendering quantum: the frame labels a block, not an ADC clock.
  const monotonicToleranceNs=Math.ceil(((receivedMs-sentMs)/2+128/sampleRate*1000)*1e6);
  const probeSentNs=Math.floor((sentMs-epochMs)*1e6);
  const probeReceivedNs=Math.ceil((receivedMs-epochMs)*1e6);
  if (![sampleIndex,monotonicNs,monotonicOriginNs,monotonicToleranceNs,probeSentNs,probeReceivedNs].every((v)=>Number.isSafeInteger(v)&&v>=0)) return null;
  return {schema:"review-capture-calibration.v1",method:"workletProbeBracket",clockId,sampleRate,firstSample,sampleIndex,monotonicNs,monotonicOriginNs,monotonicToleranceNs,probeSentNs,probeReceivedNs,performanceEpochMs:epochMs,probeSentPerformanceMs:sentMs,probeReceivedPerformanceMs:receivedMs};
}
