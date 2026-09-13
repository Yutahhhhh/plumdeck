#pragma once

#include "backend.h"
#include "performance_input.h"
#include "junction/runtime.h"
#include <QElapsedTimer>
#include <QJsonObject>
#include <QJsonArray>
#include <QObject>
#include <QTimer>
#include <array>

class Host final : public QObject {
public:
    explicit Host(std::unique_ptr<PlaybackBackend> backend);
    ~Host();
    void line(const QByteArray& line);
    void malformed(const QString& message);
private:
    using QObject::event;
    QJsonObject info() const;
    QJsonObject snapshot();
    QJsonObject envelope(const QString& kind) const;
    void send(QJsonObject message) const;
    void result(const QJsonObject& cmd, const QJsonObject& data);
    void error(const QJsonObject& cmd, const QString& code, const QString& message);
    void event(const QString& name, const QJsonObject& data);
    void resetDeck(int index);
    void releaseScratch(int index, bool finish = true);
    void publishSyncLeader(int commanded);
    static QJsonObject emptyDeck();
    void sample();
    QJsonObject sampleDeck(int index);
    QJsonObject recordingState();
    void sampleRecordingTimeline();
    void completed(int index, quint64 generation, QJsonObject metadata, QString error);
    void beginLoad(const QJsonObject& cmd, int index, const QJsonObject& descriptor);
    std::unique_ptr<PlaybackBackend> backend_;
    std::unique_ptr<junction::Runtime> junction_;
    std::unique_ptr<PerformanceInput> performanceInput_;
    QElapsedTimer clock_;
    QTimer timer_;
    QString engineId_, sessionId_;
    quint64 rev_ = 0, seq_ = 0, lastId_ = 0, generation_ = 0;
    struct DeckSlot {
        QJsonObject state, descriptor;
        quint64 generation = 0;
        quint64 reverseGesture = 0;
        bool transportTouched = false;
        QString scratchGesture;
        qint64 scratchLastInputMs = 0;
        /** 最後に受け取ったスクラッチ位置。離すときはここへ着地させる。 */
        double scratchLastPositionMs = 0;
    };
    std::array<DeckSlot, 4> slots_;
    QJsonArray recordingTimeline_;
    QString recordingTimelineKey_;
    std::array<int, 4> recordingOpenSegments_{{-1, -1, -1, -1}};
    unsigned recordingTimelineDropped_ = 0;
};
