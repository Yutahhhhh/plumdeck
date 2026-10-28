#pragma once
#include "effects/chains/standardeffectchain.h"
#include "effects/effectsmanager.h"
#include "effects/effectslot.h"
#include "effects/effectparameter.h"
#include "control/controlobject.h"
#include <QJsonObject>
#include <QJsonArray>
#include <QTimer>
#include <algorithm>
#include <cmath>

// A fifth native chain, independent from the four deck pad chains. Reuse the
// pinned Mixxx chain's protected messenger via a base-class member pointer;
// no downcast and no separate audio-thread queue or upstream modification.
class BeatFx final : public StandardEffectChain {
    static constexpr int kChain = 4; // 5th unit; the four deck pad chains are 0..3.
    static QString slotGroup() { return StandardEffectChain::formatEffectSlotGroup(kChain, 0); }
    static EffectsMessengerPointer messenger(EffectChain& chain) {
        auto member = &BeatFx::m_pMessenger;
        return chain.*member;
    }
public:
    // PITCH's LEVEL chooses transposition, not dry/wet. At zero it must still
    // output the downward-shifted signal instead of bypassing the processor.
    static double chainMix(const QString& effect, double level) {
        return effect == "pitchshift" ? 1.0 : level;
    }
    // Send-type effects keep their echo/reverb after OFF: the input send
    // closes and what is already captured decays before the chain stops.
    static bool hasTail(const QString& effect) {
        return effect == "echo" || effect == "reverb" || effect == "lowcutecho" || effect == "mtdelay" || effect == "spiral";
    }
    explicit BeatFx(EffectsManager* effects)
        : StandardEffectChain(kChain, effects, messenger(*effects->getStandardEffectChain(0))), effects_(effects) {
        loadEmptyNamelessPreset();
        for (const auto& channel : effects_->registeredInputChannels()) ControlObject::set(ConfigKey(group(), QStringLiteral("group_%1_enable").arg(channel.name())), 0);
        configure({{"effect","echo"},{"enabled",false}});
        ControlObject::set(ConfigKey(group(), "enabled"), 0);
    }
    QJsonObject state() const { QJsonObject parameters; for(const auto& map : {getEffectSlots()[0]->getLoadedParameters(),getEffectSlots()[0]->getHiddenParameters()}) for(const auto& values:map) for(const auto& value:values) parameters[value->manifest()->id()]=value->getValue(); QJsonArray routes; for(const auto& channel:getActiveChannels()) routes.append(channel.name()); return {{"releaseActive",releaseActive_},{"releaseRestore",releaseRestore_},{"parameters",parameters},{"processor",getEffectSlots()[0]->id()},{"slotEnabled",ControlObject::get(ConfigKey(slotGroup(),"enabled"))},{"routes",routes}, {"nativeEnabled",ControlObject::get(ConfigKey(group(),"enabled"))}, {"bpm",manualBpm_}, {"auto",manualBpm_==0}, {"effect", effect_}, {"target", target_}, {"enabled", enabled_}, {"tail", tail_}, {"mix", mix_}, {"beats", beats_}}; }
    QString configure(const QJsonObject& p) {
        if (p.contains("release")) {
            if (!p["release"].isBool()) return "Expected release boolean";
            if (p["release"].toBool()) {
                if (releaseActive_) return {};
                releaseRestore_ = {{"effect",effect_},{"target",target_},{"mix",mix_},{"beats",beats_},{"enabled",false}};
                const auto error = configure({{"effect","echo"},{"enabled",true},{"mix",1.0},{"beats",0.5}});
                releaseActive_ = error.isEmpty();
                return error;
            }
            if (!releaseActive_) return {};
            releaseActive_ = false;
            return configure(releaseRestore_);
        }
        // A selector/knob operation ends the temporary release effect first.
        // Its saved mix/effect must never overwrite a later normal operation.
        if (releaseActive_) {
            releaseActive_ = false;
            const auto error = configure(releaseRestore_);
            if (!error.isEmpty()) return error;
        }
        if (p.contains("toggle") && !p["toggle"].isBool()) return "Expected toggle boolean";
        const QString effect = p.value("effect").toString(effect_), target = p.value("target").toString(target_);
        const auto number = [&p](const QString& key, double low, double high) { const auto v = p.value(key); return v.isUndefined() || (v.isDouble() && std::isfinite(v.toDouble()) && v.toDouble() >= low && v.toDouble() <= high); };
        if (!number("mix",0,1) || !number("beats",1.0/16,16) || (p.contains("enabled") && !p["enabled"].isBool())) return "Invalid beat FX mix, beats or enabled";
        if (target != "A" && target != "B" && target != "C" && target != "D" && target != "master" && target != "mic" && target != "sampler") return "Invalid beat FX target";
        if (p.contains("bpm") && (!number("bpm",40,300))) return "Expected BPM between 40 and 300";
        if (p.contains("auto") && !p["auto"].isBool()) return "Expected auto boolean";
        const auto manifest = effects_->getBackendManager()->getManifest("org.plumdeck.effects." + effect, EffectBackendType::BuiltIn);
        if (!manifest) return "Beat FX processor is unavailable: " + effect;
        const bool sounding = ControlObject::get(ConfigKey(group(), "enabled")) > 0 && !tail_;
        target_ = target; mix_ = p.value("mix").toDouble(mix_); beats_ = p.value("beats").toDouble(beats_); enabled_ = p.value("enabled").toBool(enabled_);
        if (p["toggle"].toBool()) enabled_ = !enabled_;
        if (p.contains("bpm")) manualBpm_=p["bpm"].toDouble();
        if (p["auto"].toBool()) manualBpm_=0;
        // EffectSlot only forwards its enable flag to the audio thread from its
        // own valueChanged handler and from the tail of loadEffectInner().
        // EffectSlot::setEnabled() writes through the slot's own ControlObject,
        // which names itself as the sender, and ControlObject suppresses
        // valueChanged for its own writes — so setEnabled() alone never reaches
        // the engine and the effect stays silent. Publish it through the static
        // setter, before any load, so the load path and the no-load path both
        // carry the value we asked for.
        // A repeated OFF during a tail keeps the running tail and its timer.
        const bool wasTail = tail_, tailing = !enabled_ && effect == effect_ && hasTail(effect) && (sounding || wasTail);
        const auto generation = tailing && wasTail ? tailGeneration_ : ++tailGeneration_;
        tail_ = false;
        ControlObject::set(ConfigKey(slotGroup(), "enabled"), enabled_ || tailing ? 1 : 0);
        if (effect_ != effect || !getEffectSlot(0)->isLoaded()) { getEffectSlot(0)->loadEffectWithDefaults(effects_->getBackendManager()->getManifest(effect == "reverb" ? "org.mixxx.effects.echo" : "org.mixxx.effects.reverb",EffectBackendType::BuiltIn)); getEffectSlot(0)->loadEffectWithDefaults(manifest); effect_ = effect; }
        for (const auto& channel : effects_->registeredInputChannels()) {
            const auto name = channel.name();
            // MasterOutput is hardware-only, after recording and monitor taps.
            // BEAT FX belongs on the common master bus so those hear it too.
            const bool chosen = target == "master" ? name == "[Master]" : target == "mic" ? name == "[Microphone1]" : target == "sampler" ? name.startsWith("[Sampler") : name == QStringLiteral("[Channel%1]").arg(target[0].unicode() - 'A' + 1);
            ControlObject::set(ConfigKey(group(), QStringLiteral("group_%1_enable").arg(name)), chosen ? 1 : 0);
        }
        getEffectSlot(0)->setMetaParameter(effect == "pitchshift" ? mix_ : 1.0, true);
        for (const auto& map : {getEffectSlot(0)->getLoadedParameters(),getEffectSlot(0)->getHiddenParameters()}) for (const auto& parameters : map) for (const auto& parameter : parameters) {
            const auto id = parameter->manifest()->id();
            if (id == "manual_bpm") { parameter->setValue(manualBpm_); parameter->updateEngineState(); }
            if (id == "depth") { parameter->setValue(mix_); parameter->updateEngineState(); }
            if (effect == "tremolo" && id == "waveform") { parameter->setValue(.005); parameter->updateEngineState(); }
            // PITCH: the macro also drives "range", which folds LEVEL 0 back to
            // no shift. Fix the range so LEVEL spans -1..+1 octave.
            if (effect == "pitchshift" && id == "range") { parameter->setValue(1); parameter->updateEngineState(); }
            // OFF closes the send for the tail; ON reopens it (a re-press
            // during a tail does not reload, so the macro alone cannot).
            if (id == "send_amount" && hasTail(effect) && (tailing || enabled_)) {
                parameter->setValue(tailing ? 0 : parameter->manifest()->getMaximum()); parameter->updateEngineState();
            }
            if (id == "beats" || id == "delay_time" || id == "lfo_period" || id == "speed" || (effect == "tremolo" && id == "rate")) {
                const double requested = id == "rate" ? 1.0/beats_ : beats_;
                const double applied = qBound(parameter->manifest()->getMinimum(), requested, parameter->manifest()->getMaximum());
                parameter->setValue(applied); parameter->updateEngineState();
                if (id == "rate") beats_ = 1.0/applied; else beats_ = applied;
            }
        }
        ControlObject::set(ConfigKey(group(),"mix"),chainMix(effect_,mix_));
        if (tailing) {
            tail_ = true;
            if (!wasTail) QTimer::singleShot(tailMs(), this, [this, generation] {
                if (tailGeneration_ != generation) return;
                tail_ = false;
                ControlObject::set(ConfigKey(slotGroup(), "enabled"), 0);
                ControlObject::set(ConfigKey(group(), "enabled"), 0);
            });
            return {};
        }
        ControlObject::set(ConfigKey(group(),"enabled"),enabled_ ? 1 : 0);
        return {};
    }
    void stop() { ++tailGeneration_; tail_ = false; if(releaseActive_) configure({{"release",false}}); enabled_ = false; ControlObject::set(ConfigKey(slotGroup(),"enabled"),0); ControlObject::set(ConfigKey(group(),"enabled"),0); }
private:
    EffectsManager* effects_;
    QString effect_ = "echo", target_ = "A";
    bool enabled_ = false;
    bool releaseActive_ = false;
    QJsonObject releaseRestore_;
    double mix_ = 0.5, beats_ = 1, manualBpm_ = 0;
    bool tail_ = false;
    quint64 tailGeneration_ = 0;
    /** Echo: eight repeats of the delay (at least two beats). Reverb: eight beats. */
    int tailMs() const {
        double bpm = manualBpm_;
        if (bpm <= 0 && target_.size() == 1) bpm = ControlObject::get(ConfigKey(QStringLiteral("[Channel%1]").arg(target_[0].unicode() - 'A' + 1), "bpm"));
        if (!std::isfinite(bpm) || bpm < 20) bpm = 120;
        const double beats = effect_ == "reverb" ? 8 : std::max(2.0, beats_ * 8);
        return int(std::clamp(60000 * beats / bpm, 500.0, 16000.0));
    }
};
