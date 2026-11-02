import { captureCalibration } from "./clock";

test("sample/monotonic calibration uses a causal bracket, never packet arrival as sample zero", () => {
  const input={clockId:"mic_epoch_0",sampleRate:48000,firstSample:0,absoluteFrame:96000,originFrame:48000,epochMs:1000,sentMs:2010,receivedMs:2014};
  const result=captureCalibration(input)!;
  expect(result.sampleIndex).toBe(48000);
  expect(result.monotonicNs).toBe(1_012_000_000);
  expect(result.monotonicOriginNs).toBe(12_000_000);
  expect(result.monotonicToleranceNs).toBeGreaterThanOrEqual(4_666_666);
  expect(result.monotonicToleranceNs).toBeLessThan(5_000_000);
  expect(result.probeSentNs).toBe(1_010_000_000);
  expect(result.probeReceivedNs).toBe(1_014_000_000);
  expect(captureCalibration({...input,receivedMs:input.sentMs+501})).toBeNull();
  expect(captureCalibration({...input,receivedMs:100})).toBeNull();
  expect(captureCalibration({...input,absoluteFrame:0})).toBeNull();
  expect(captureCalibration({...input,sampleRate:0})).toBeNull();
});
