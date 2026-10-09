// Phase 58: speech-to-text — uniform recorder interface.
//
// Two backends:
//   webspeech — uses window.SpeechRecognition directly (Chromium /
//               WebView2 ships it; no audio buffer round-trip, but
//               Chrome streams to Google servers behind the scenes,
//               which is why the Local option exists).
//   local     — records via MediaRecorder (audio/webm), then POSTs
//               the bytes to the user's configurable endpoint via
//               the stt_transcribe_local tauri command.
//
// Both backends expose the same `start()` / `stop()` lifecycle and
// resolve a Promise<string> with the transcribed text. The caller
// (App.tsx push-to-talk handler) doesn't need to branch on backend.

import { backend } from "./backend";

export type SttBackend = "webspeech" | "local";

// Browser-level types for SpeechRecognition aren't in the default
// lib.dom.d.ts because they're not in the WHATWG standard. Minimal
// shim — only the surface we actually call.
interface SpeechRecognitionEvent {
  results: ArrayLike<ArrayLike<{ transcript: string }>>;
}
interface SpeechRecognitionErrorEvent {
  error: string;
  message?: string;
}
interface SpeechRecognitionLike {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  start(): void;
  stop(): void;
  abort(): void;
  onresult: ((e: SpeechRecognitionEvent) => void) | null;
  onerror: ((e: SpeechRecognitionErrorEvent) => void) | null;
  onend: (() => void) | null;
}
type SpeechRecognitionCtor = new () => SpeechRecognitionLike;

function pickSpeechRecognitionCtor(): SpeechRecognitionCtor | null {
  // Chromium ships under the webkit prefix.
  const w = window as unknown as {
    SpeechRecognition?: SpeechRecognitionCtor;
    webkitSpeechRecognition?: SpeechRecognitionCtor;
  };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

export interface SttRecorder {
  /**
   * Start capturing. Resolves the next time `stop()` is called with
   * the transcribed text. Rejects on permission denial, no-mic, or
   * backend error. Silence resolves with an empty string.
   */
  start(): Promise<string>;
  /**
   * Stop the active capture. Subsequent calls are no-ops.
   */
  stop(): void;
}

/** Webspeech backend. */
function makeWebspeechRecorder(language: string): SttRecorder {
  const Ctor = pickSpeechRecognitionCtor();
  if (!Ctor) {
    return {
      start: () =>
        Promise.reject(
          new Error("Web Speech API not available — try the Local backend."),
        ),
      stop: () => {},
    };
  }
  let rec: SpeechRecognitionLike | null = null;
  let resolveText: ((s: string) => void) | null = null;
  let rejectText: ((e: Error) => void) | null = null;
  let finalText = "";

  return {
    start() {
      return new Promise<string>((resolve, reject) => {
        rec = new Ctor();
        // The Web Speech API uses BCP-47 like "he-IL" / "en-US".
        // "auto" isn't a standard value — fall back to undefined to
        // let the browser pick the system locale.
        rec.lang = language === "auto" ? "" : language;
        rec.continuous = false;
        rec.interimResults = false;
        finalText = "";
        resolveText = resolve;
        rejectText = reject;
        rec.onresult = (e) => {
          // Concatenate the (only) result's first alternative.
          for (let i = 0; i < e.results.length; i++) {
            const alt = e.results[i][0];
            if (alt?.transcript) finalText += alt.transcript;
          }
        };
        rec.onerror = (e) => {
          // "no-speech" + "aborted" are not real errors — they fire
          // when the user releases push-to-talk without speaking.
          if (e.error === "no-speech" || e.error === "aborted") {
            resolveText?.("");
            resolveText = null;
            rejectText = null;
            return;
          }
          rejectText?.(new Error(`SpeechRecognition: ${e.error}`));
          resolveText = null;
          rejectText = null;
        };
        rec.onend = () => {
          if (resolveText) {
            resolveText(finalText.trim());
            resolveText = null;
            rejectText = null;
          }
        };
        try {
          rec.start();
        } catch (e) {
          rejectText?.(new Error(`SpeechRecognition.start: ${String(e)}`));
        }
      });
    },
    stop() {
      try {
        rec?.stop();
      } catch {
        // ignore — stop on an already-ended rec throws InvalidStateError
      }
    },
  };
}

/** Local-endpoint backend. */
function makeLocalRecorder(language: string): SttRecorder {
  let stream: MediaStream | null = null;
  let recorder: MediaRecorder | null = null;
  const chunks: BlobPart[] = [];
  let resolveText: ((s: string) => void) | null = null;
  let rejectText: ((e: Error) => void) | null = null;
  // Phase 59.B: stop() can be called before getUserMedia resolves
  // (fast keydown → keyup race when the user taps the PTT key faster
  // than the mic permission prompt + stream open takes). Without
  // this flag, .stop() would no-op on the still-null recorder, then
  // getUserMedia would resolve and start a recording NOBODY would
  // ever stop — mic stays open indefinitely. The flag is checked at
  // each async boundary inside start().
  let stopRequested = false;

  const cleanup = () => {
    try {
      recorder?.stop();
    } catch {
      // ignore
    }
    for (const t of stream?.getTracks() ?? []) {
      try {
        t.stop();
      } catch {
        // ignore
      }
    }
    stream = null;
    recorder = null;
  };

  return {
    async start() {
      return new Promise<string>((resolve, reject) => {
        resolveText = resolve;
        rejectText = reject;
        stopRequested = false;
        navigator.mediaDevices
          .getUserMedia({ audio: true })
          .then((s) => {
            stream = s;
            // Phase 59.B: if stop() fired before the mic opened, tear
            // the just-acquired stream down immediately and resolve
            // with an empty string. Empty string is the agreed
            // "nothing to paste" signal — App.tsx already gates
            // pasteIntoActiveTerminal on text.length > 0.
            if (stopRequested) {
              resolveText?.("");
              cleanup();
              resolveText = null;
              rejectText = null;
              return;
            }
            // audio/webm with opus is the universally supported
            // MediaRecorder output on Chromium. Whisper accepts it
            // (via ffmpeg) and whisper.cpp's server transcodes
            // internally too.
            const mime = "audio/webm;codecs=opus";
            recorder = new MediaRecorder(s, {
              mimeType: MediaRecorder.isTypeSupported(mime)
                ? mime
                : "audio/webm",
            });
            recorder.ondataavailable = (e) => {
              if (e.data && e.data.size > 0) chunks.push(e.data);
            };
            recorder.onstop = async () => {
              try {
                const blob = new Blob(chunks, { type: "audio/webm" });
                chunks.length = 0;
                if (blob.size === 0) {
                  resolveText?.("");
                  cleanup();
                  return;
                }
                const buf = new Uint8Array(await blob.arrayBuffer());
                // Tauri command expects Vec<u8>; the @tauri-apps/api
                // serializer turns a number[] into that shape. We pay
                // an n*2-ish copy here but the payloads cap at ~30s
                // of audio (a few MB), well under any pipe limit.
                const text = await backend.call<string>("stt_transcribe_local", {
                  audioBytes: Array.from(buf),
                  language: language === "auto" ? "" : language,
                });
                resolveText?.(text.trim());
              } catch (e) {
                rejectText?.(new Error(String(e)));
              } finally {
                cleanup();
                resolveText = null;
                rejectText = null;
              }
            };
            recorder.start();
          })
          .catch((e) => {
            rejectText?.(new Error(`getUserMedia: ${String(e)}`));
            cleanup();
            resolveText = null;
            rejectText = null;
          });
      });
    },
    stop() {
      stopRequested = true;
      try {
        recorder?.stop();
      } catch {
        // ignore — stop without start, or already stopped
      }
    },
  };
}

export function makeSttRecorder(
  backend: SttBackend,
  language: string,
): SttRecorder {
  return backend === "local"
    ? makeLocalRecorder(language)
    : makeWebspeechRecorder(language);
}
