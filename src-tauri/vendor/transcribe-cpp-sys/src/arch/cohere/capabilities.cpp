// arch/cohere/capabilities.cpp - Cohere ASR capability defaults.

#include "cohere.h"

namespace transcribe::cohere {

void apply_family_invariants(transcribe_model & model) {
    transcribe_capabilities & caps = model.caps;

    caps.native_sample_rate = 16000;
    caps.supports_translate = false;

    // <|notimestamp|> is hard-wired, so the token stream carries no
    // alignment info: emit text-only with zeroed timings, advertise NONE.
    caps.max_timestamp_kind = TRANSCRIBE_TIMESTAMPS_NONE;

    transcribe::set_feature(&model, TRANSCRIBE_FEATURE_CANCELLATION, true);
    // No trained prompt/context input; custom vocabulary is applied as
    // decode-time keyword boosting in the single-utterance greedy path.
    transcribe::set_feature(&model, TRANSCRIBE_FEATURE_KEYWORD_BOOST, true);
}

}  // namespace transcribe::cohere
