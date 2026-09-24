// transcribe-bias.cpp - decode-time keyword boosting. See transcribe-bias.h.

#include "transcribe-bias.h"

#include "transcribe-log.h"

#include <algorithm>
#include <cctype>
#include <limits>

namespace transcribe {

namespace {

// SentencePiece word-boundary marker (U+2581).
const char * const k_sp_space = "\xE2\x96\x81";

bool is_utf8_continuation(unsigned char c) {
    return (c & 0xC0) == 0x80;
}

std::string trim(const std::string & s) {
    size_t b = 0;
    size_t e = s.size();
    while (b < e && std::isspace(static_cast<unsigned char>(s[b]))) {
        ++b;
    }
    while (e > b && std::isspace(static_cast<unsigned char>(s[e - 1]))) {
        --e;
    }
    return s.substr(b, e - b);
}

// ASCII-only first-letter capitalization; non-ASCII phrases keep one form.
std::string capitalize_first(const std::string & s) {
    std::string out = s;
    if (!out.empty() && out[0] >= 'a' && out[0] <= 'z') {
        out[0] = static_cast<char>(out[0] - 'a' + 'A');
    }
    return out;
}

// Viterbi segmentation of `text` into vocab pieces maximizing the summed
// unigram scores (or, without scores, minimizing the piece count).
bool encode_sentencepiece(const Tokenizer & tok, const std::string & text, std::vector<int32_t> & out) {
    const size_t               n      = text.size();
    const std::vector<float> & scores = tok.scores();
    constexpr size_t           k_max_piece_bytes = 48;
    const double               neg_inf           = -std::numeric_limits<double>::infinity();

    std::vector<double>  best(n + 1, neg_inf);
    std::vector<int32_t> best_id(n + 1, -1);
    std::vector<size_t>  best_from(n + 1, 0);
    best[0] = 0.0;

    for (size_t i = 0; i < n; ++i) {
        if (best[i] == neg_inf || is_utf8_continuation(static_cast<unsigned char>(text[i]))) {
            continue;
        }
        const size_t limit = std::min(n, i + k_max_piece_bytes);
        for (size_t j = i + 1; j <= limit; ++j) {
            if (j < n && is_utf8_continuation(static_cast<unsigned char>(text[j]))) {
                continue;
            }
            const int id = tok.find(text.substr(i, j - i));
            if (id < 0 || tok.is_control(id)) {
                continue;
            }
            const double s =
                best[i] + (static_cast<size_t>(id) < scores.size() ? static_cast<double>(scores[id]) : -1.0);
            if (s > best[j]) {
                best[j]      = s;
                best_id[j]   = id;
                best_from[j] = i;
            }
        }
    }

    if (best[n] == neg_inf) {
        return false;
    }
    out.clear();
    for (size_t j = n; j > 0; j = best_from[j]) {
        out.push_back(best_id[j]);
    }
    std::reverse(out.begin(), out.end());
    return true;
}

}  // namespace

bool encode_for_bias(const Tokenizer & tok, const std::string & text, std::vector<int32_t> & out) {
    out.clear();
    if (text.empty()) {
        return false;
    }
    if (tok.model_type() == "gpt2") {
        return tok.encode(text, out) == TRANSCRIBE_OK && !out.empty();
    }
    return encode_sentencepiece(tok, text, out);
}

void KeywordBooster::add_sequence(const std::vector<int32_t> & ids) {
    int node = 0;
    for (const int32_t id : ids) {
        int next = child(node, id);
        if (next == 0) {
            next = static_cast<int>(nodes_.size());
            nodes_.push_back(Node{});
            nodes_[node].next.emplace_back(id, next);
        }
        node = next;
    }
    nodes_[node].terminal = true;
}

int KeywordBooster::child(int node, int32_t token) const {
    for (const auto & [tok, next] : nodes_[node].next) {
        if (tok == token) {
            return next;
        }
    }
    return 0;
}

void KeywordBooster::build(const Tokenizer & tok, const char * phrases, float strength, float family_default) {
    nodes_.assign(1, Node{});
    state_.clear();
    n_vocab_  = tok.n_tokens();
    strength_ = strength > 0.0f ? strength : family_default;
    if (phrases == nullptr) {
        return;
    }

    const bool  gpt2 = tok.model_type() == "gpt2";
    std::string all(phrases);
    size_t      start   = 0;
    int         n_added = 0;
    while (start <= all.size()) {
        size_t end = all.find('\n', start);
        if (end == std::string::npos) {
            end = all.size();
        }
        const std::string phrase = trim(all.substr(start, end - start));
        start                    = end + 1;
        if (phrase.empty()) {
            continue;
        }

        std::vector<std::string> surfaces;
        for (const std::string & form : {phrase, capitalize_first(phrase)}) {
            if (gpt2) {
                surfaces.push_back(" " + form);  // mid-text word
                surfaces.push_back(form);        // start of text
            } else {
                std::string sp = k_sp_space;
                for (const char c : form) {
                    if (c == ' ') {
                        sp += k_sp_space;
                    } else {
                        sp += c;
                    }
                }
                surfaces.push_back(sp);
            }
        }

        bool encoded = false;
        for (const std::string & surface : surfaces) {
            std::vector<int32_t> ids;
            if (encode_for_bias(tok, surface, ids)) {
                add_sequence(ids);
                encoded = true;
            }
        }
        if (encoded) {
            ++n_added;
        } else {
            log_msg(TRANSCRIBE_LOG_LEVEL_WARN, "keyword boost: could not tokenize phrase '%s'; skipped",
                    phrase.c_str());
        }
    }
    log_msg(TRANSCRIBE_LOG_LEVEL_DEBUG, "keyword boost: %d phrase(s), %d trie nodes, strength %.2f", n_added,
            static_cast<int>(nodes_.size()), static_cast<double>(strength_));
}

void KeywordBooster::observe(int32_t token) {
    std::vector<int> next_state;
    bool             matched = false;
    auto             advance = [&](int node) {
        const int c = child(node, token);
        matched     = matched || c != 0;
        if (c != 0 && !nodes_[c].next.empty() &&
            std::find(next_state.begin(), next_state.end(), c) == next_state.end()) {
            next_state.push_back(c);
        }
    };
    advance(0);
    for (const int node : state_) {
        advance(node);
    }
    state_          = std::move(next_state);
    just_completed_ = matched;
}

template <typename F> void KeywordBooster::for_each_boost(F && f) const {
    if (!just_completed_) {
        const float start_bonus = 0.5f * strength_;
        for (const auto & [tok, next] : nodes_[0].next) {
            f(tok, start_bonus);
        }
    }
    for (const int node : state_) {
        for (const auto & [tok, next] : nodes_[node].next) {
            f(tok, strength_);
        }
    }
}

void KeywordBooster::fill(std::vector<float> & out, int n) const {
    out.assign(static_cast<size_t>(std::max(n, 0)), 0.0f);
    for_each_boost([&](int32_t tok, float bonus) {
        if (tok >= 0 && tok < n) {
            out[tok] = std::max(out[tok], bonus);
        }
    });
}

void KeywordBooster::apply(float * logits, int n) const {
    std::vector<float> bias;
    fill(bias, n);
    for (int i = 0; i < n; ++i) {
        logits[i] += bias[i];
    }
}

}  // namespace transcribe
