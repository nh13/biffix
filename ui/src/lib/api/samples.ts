// Sample detection: files → samples → samplesheet rows (biffix_api::SamplesContract).

import type { SampleDetectRequest, DetectedSamples } from '#lib/generated/samples.js';
import { API_PATHS, requestJson } from './client';

export type { SampleDetectRequest, DetectedSamples };

/** Group files into samples (and, given the workflow's samplesheet, its rows). */
export function detectSamples(body: SampleDetectRequest): Promise<DetectedSamples> {
  return requestJson<DetectedSamples>(API_PATHS.samplesDetect, { method: 'POST', body });
}
