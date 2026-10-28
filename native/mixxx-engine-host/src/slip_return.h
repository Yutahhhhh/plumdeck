#pragma once

// Cancel a pending off/on Slip return before starting a new gesture. If the
// old return already switched the control off, re-arm it for the new gesture.
template<typename Generation>
bool interruptSlipReturn(Generation& generation, bool wanted, bool enabled) {
    ++generation;
    return wanted && !enabled;
}
