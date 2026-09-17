#pragma once
#include <array>
#include <atomic>
#include <cmath>
#include <QFileInfo>
#include <QJsonArray>
#include <QObject>
#include "control/controlobject.h"
#include "engine/channels/enginedeck.h"
#include "engine/enginebuffer.h"
#include "engine/enginemixer.h"
#include "track/track.h"

// Main-thread commands, native audio callbacks. EngineMixer owns the channels;
// this bank owns their metadata and uses a QObject context for async load guards.
class SamplerBank final : public QObject {
public:
    SamplerBank(UserSettingsPointer settings, EngineMixer* mixer, EffectsManager* effects) {
        for (int i = 0; i < 64; ++i) {
            auto& s = slots_[i];
            s.group = QStringLiteral("[Sampler%1]").arg(i + 1);
            s.deck = new EngineDeck(mixer->registerChannelGroup(s.group), settings, mixer, effects, EngineChannel::CENTER, false);
            mixer->addChannel(s.deck);
            ControlObject::set(ConfigKey(s.group, "main_mix"), 1);
            ControlObject::set(ConfigKey(s.group, "volume"), gain_);
            QObject::connect(s.deck->getEngineBuffer(), &EngineBuffer::trackLoaded, this, [this, i](TrackPointer track, TrackPointer) {
                auto& slot = slots_[i];
                if (!track || track != slot.track) return;
                slot.ready = true; slot.error.clear();

            }, Qt::QueuedConnection);
            QObject::connect(s.deck->getEngineBuffer(), &EngineBuffer::trackLoadFailed, this, [this, i](TrackPointer track, const QString& error) {
                auto& slot = slots_[i];
                if (track != slot.track) return;
                slot.ready = false; slot.error = error;
            }, Qt::QueuedConnection);
        }
    }
    QJsonObject state() const {
        QJsonArray items;
        for (int i = 0; i < 16; ++i) {
            const auto& s = slots_[bank_ * 16 + i];
            items.append(QJsonObject{{"slot", i}, {"path", s.path}, {"name", QFileInfo(s.path).fileName()},
                {"status", s.path.isEmpty() ? "empty" : !s.error.isEmpty() ? "error" : !s.ready ? "loading" : ControlObject::get(ConfigKey(s.group, "play")) > 0 ? "playing" : "ready"},
                {"error", s.error}, {"durationMs", s.ready ? s.track->getDuration() * 1000 : 0}, {"revision", static_cast<double>(s.revision)}});
        }
        return {{"slots", items}, {"gain", gain_}, {"pfl", pfl_}, {"bank", bank_}};
    }
    QString command(const QString& op, const QJsonObject& params) {
        if (op == "sampler.state") return {};
        if (op == "sampler.bank") {
            const auto value = params["bank"];
            if (!value.isDouble() || value.toDouble() != std::floor(value.toDouble()) || value.toDouble() < 0 || value.toDouble() > 3) return "Expected sampler bank 0..3";
            bank_ = value.toInt(); return {};
        }
        if (op == "sampler.stopAll") { stopAll(); return {}; }
        if (op == "sampler.gain" || op == "sampler.pfl") {
            if (op == "sampler.gain") {
                const auto v = params["gain"];
                if (!v.isDouble() || !std::isfinite(v.toDouble()) || v.toDouble() < 0 || v.toDouble() > 1) return "Expected sampler gain 0..1";
                gain_ = v.toDouble();
            } else {
                if (!params["enabled"].isBool()) return "Expected sampler pfl boolean";
                pfl_ = params["enabled"].toBool();
            }
            for (const auto& s : slots_) { ControlObject::set(ConfigKey(s.group, "volume"), gain_); ControlObject::set(ConfigKey(s.group, "pfl"), pfl_ ? 1 : 0); }
            return {};
        }
        const auto index = params["slot"];
        if (!index.isDouble() || !std::isfinite(index.toDouble()) || index.toDouble() != std::floor(index.toDouble()) || index.toDouble() < 0 || index.toDouble() >= 16) return "Expected sampler slot 0..15";
        int targetBank = bank_;
        if (params.contains("bank")) {
            const auto value = params["bank"];
            if (!value.isDouble() || value.toDouble() != std::floor(value.toDouble()) || value.toDouble() < 0 || value.toDouble() > 3) return "Expected sampler bank 0..3";
            targetBank = value.toInt();
        }
        auto& s = slots_[targetBank * 16 + index.toInt()];
        if (op == "sampler.load") {
            const auto path = params["path"].toString();
            if (path.size() > 4096 || !QFileInfo(path).isAbsolute() || !QFileInfo(path).isFile()) return "Expected an existing absolute sample file path";
            ControlObject::set(ConfigKey(s.group, "play"), 0);
            ++s.revision; s.path = path; s.ready = false; s.error.clear();
            s.track = Track::newTemporary(path);
            s.deck->getEngineBuffer()->loadTrack(s.track, false, nullptr);
            return {};
        }
        if (op == "sampler.eject") {
            ControlObject::set(ConfigKey(s.group, "play"), 0);
            ++s.revision; s.track.reset(); s.path.clear(); s.error.clear(); s.ready = false;
            s.deck->getEngineBuffer()->ejectTrack(); return {};
        }
        if (!params["revision"].isUndefined() && params["revision"].toDouble(-1) != static_cast<double>(s.revision)) return "Sample changed; stale pad gesture";
        if (op == "sampler.stop") { ControlObject::set(ConfigKey(s.group, "play"), 0); return {}; }
        if (op == "sampler.play") {
            if (!s.ready || !s.error.isEmpty()) return "Sample is not ready";
            ControlObject::set(ConfigKey(s.group, "playposition"), 0);
            ControlObject::set(ConfigKey(s.group, "play"), 1); return {};
        }
        return "Unknown sampler operation";
    }
    void stopAll() { for (const auto& s : slots_) ControlObject::set(ConfigKey(s.group, "play"), 0); }
private:
    struct Slot { QString group, path, error; EngineDeck* deck = nullptr; TrackPointer track; bool ready = false; quint64 revision = 0; };
    std::array<Slot, 64> slots_;
    int bank_ = 0;
    double gain_ = 0.7;
    bool pfl_ = false;
};
