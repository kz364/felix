#!/usr/bin/env python3
"""Generate the custom-vocabulary eval set used to calibrate keyword boosting.

    python3 scripts/bias-eval-clips.py <out_dir>
    cd src-tauri && cargo run --release --example bias_eval -- <model.gguf> <out_dir>/manifest.json 0 4 8 12

Synthesizes 40 clips with macOS `say` (two voices): 12 sentences containing
vocabulary terms and 8 controls, several full of sound-alikes (Casper, queen,
whispered, tower, cohort) to catch false insertions. TTS is a stand-in for
real speech; re-run with your own recordings for a closer calibration.
"""
import json
import os
import subprocess
import sys

VOCAB = ["Rivera", "Lumenfold", "kubectl", "Qwen", "Parakeet", "Tauri",
         "Wispr", "Nemotron", "Cohere", "Priya", "Supabase", "Zustand"]
POSITIVE = [
    "Please run kubectl apply, then ping Rivera about the Lumenfold deploy.",
    "I asked Qwen to review the Tauri build before lunch.",
    "Priya thinks Parakeet is faster than Nemotron on short clips.",
    "Move the auth tables into Supabase and keep the UI state in Zustand.",
    "Wispr sends audio to the cloud, but Cohere runs locally here.",
    "Rivera and Priya will demo the Lumenfold dashboard on Friday.",
    "Check whether kubectl can reach the cluster from Rivera's laptop.",
    "The Tauri app talks to Supabase through a small Rust service.",
    "Nemotron and Parakeet both come from the same research group.",
    "Can Qwen summarize the notes Priya left in the Lumenfold channel?",
    "Zustand keeps the settings store simple compared to Redux.",
    "Cohere released a new transcription model this month.",
]
CONTROL = [
    "My cousin Casper said the handyman will fix the cube shelf on Wednesday.",
    "Can you hand me the cube and the feather? I think it will rain later.",
    "He whispered that the queen would arrive at noon.",
    "The parrot and the pigeon sat together on the tower.",
    "Our cohort of new hires starts on Monday at the downtown office.",
    "The quick brown fox jumps over the lazy dog, and then it quietly went home.",
    "Please send the invoice to the finance team before Friday.",
    "I'd like a cup of tea with honey and a slice of lemon, please.",
]
# How TTS should pronounce invented spellings; references keep the real ones.
SAY_AS = {"kubectl": "cube control", "Qwen": "Chwen", "Wispr": "Whisper",
          "Tauri": "Towry", "Zustand": "Zoo-shtand", "Supabase": "Soopa-base",
          "Priya": "Pree-ya", "Cohere": "Co-here"}
VOICES = ["Samantha", "Daniel"]


def main() -> None:
    out = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "bias-eval")
    os.makedirs(os.path.join(out, "wav"), exist_ok=True)
    clips = []
    for voice in VOICES:
        for kind, sentences in (("pos", POSITIVE), ("ctrl", CONTROL)):
            for i, ref in enumerate(sentences):
                spoken = ref
                if kind == "pos":
                    for word, sound in SAY_AS.items():
                        spoken = spoken.replace(word, sound)
                base = os.path.join(out, "wav", f"{kind}{i:02d}_{voice}")
                subprocess.run(["say", "-v", voice, "-o", base + ".aiff", spoken], check=True)
                subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1",
                                base + ".aiff", base + ".wav"], check=True)
                os.remove(base + ".aiff")
                clips.append({"wav": base + ".wav", "ref": ref, "kind": kind})
    with open(os.path.join(out, "manifest.json"), "w") as f:
        json.dump({"vocab": VOCAB, "clips": clips}, f, indent=1)
    print(f"{len(clips)} clips -> {out}/manifest.json")


if __name__ == "__main__":
    main()
