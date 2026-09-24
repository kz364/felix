// transcribe-bias.h - decode-time keyword boosting (shallow-fusion style).
//
// INTERNAL. Families whose decoders have no trained prompt/context input
// (Cohere Transcribe, Canary-Qwen) bias toward user phrases at decode time:
// each phrase is tokenized with the model's own tokenizer into a prefix trie,
// and before every greedy argmax the logits of tokens that start a phrase
// (half strength) or continue a partially emitted one (full strength) get a
// fixed bonus. With greedy decoding no rescoring is needed when a partial
// match is abandoned; the state simply falls back to the root. Phrase starts
// are not boosted directly after a phrase token, which prevents runaways.
// Logit scales differ per model, so each family passes its own default
// strength, calibrated with src-tauri/examples/bias_eval.rs (40 clips, 12-term
// vocabulary): ~60% of the strength at which decoding first runs away, with no
// dropped content and no false insertions beyond true homophones.

#pragma once

#include "transcribe-tokenizer.h"

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

namespace transcribe {

constexpr float k_default_bias_strength = 6.0f;  // fallback for uncalibrated families

class KeywordBooster {
  public:
    // Build from newline-separated phrases. Each phrase is added in a few
    // surface variants (word-initial and text-initial, original and
    // capitalized first letter). Phrases the tokenizer can't encode are
    // skipped. strength <= 0 selects the family's calibrated default.
    void build(const Tokenizer & tok,
               const char *      phrases,
               float             strength,
               float             family_default = k_default_bias_strength);

    bool active() const { return nodes_.size() > 1; }

    int n_vocab() const { return n_vocab_; }

    // Start of a decode: no partial matches.
    void reset() {
        state_.clear();
        just_completed_ = false;
    }

    // Advance partial matches with the token just emitted.
    void observe(int32_t token);

    // Bias for the next step: out is resized to n (the logits width, which
    // may exceed the tokenizer vocab when the head is padded) and overwritten.
    void fill(std::vector<float> & out, int n) const;

    // Add the next-step bias to host logits in place.
    void apply(float * logits, int n) const;

  private:
    struct Node {
        std::vector<std::pair<int32_t, int>> next;  // (token, child node)
        bool                                 terminal = false;  // a phrase ends here
    };

    int  child(int node, int32_t token) const;
    void add_sequence(const std::vector<int32_t> & ids);
    template <typename F> void for_each_boost(F && f) const;

    std::vector<Node> nodes_{Node{}};  // nodes_[0] is the root
    std::vector<int>  state_;          // active non-root nodes
    // The last token belonged to a phrase (started, continued or finished
    // one): skip phrase-start bonuses for one step so boosted tokens can't
    // chain into runaways ("Qwen Qwen Qwen", "Kub Kub Kub").
    bool              just_completed_ = false;
    float             strength_ = k_default_bias_strength;
    int               n_vocab_  = 0;
};

// Tokenize `text` as the model would emit it, for biasing. "gpt2" uses the
// tokenizer's BPE encoder; SentencePiece ("unigram"/"bpe") uses a Viterbi
// segmentation over the vocab scores. Returns false if the text can't be
// covered by vocab pieces.
bool encode_for_bias(const Tokenizer & tok, const std::string & text, std::vector<int32_t> & out);

}  // namespace transcribe
