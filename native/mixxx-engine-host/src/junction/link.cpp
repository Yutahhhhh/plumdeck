#include "link.h"
#include "../backend.h"
#include <QCryptographicHash>
#include <QHostAddress>
#include <QJsonArray>
#include <QTcpServer>
#include <QTcpSocket>
#include <QUrl>
#include <QUrlQuery>
#include <QUuid>
#include <samplerate.h>
#include <algorithm>
#include <cmath>
#include <cstring>
namespace junction {
namespace {
constexpr int kHeaderBytes=16;
constexpr quint8 kKindInput=1,kKindProgram=2,kKindUplink=3;
constexpr qsizetype kMaxMessage=1<<20;
// Never queue more than ~0.5 s of uplink on a stalled socket: the WebView
// would only have to throw it away.
constexpr qint64 kMaxBacklogBytes=48000*8/2+4096;
template<typename T> T readLe(const char* p){T v;std::memcpy(&v,p,sizeof v);return v;}
}

Link::Link(PlaybackBackend* backend,QObject* parent):QObject(parent),backend_(backend) {
    int error=0;
    uplinkSrc_=src_new(SRC_SINC_MEDIUM_QUALITY,2,&error);
    uplinkIn_.resize(4096*2);
    uplinkOut_.resize(4096*2*2);
    pumpTimer_.setInterval(5);
    connect(&pumpTimer_,&QTimer::timeout,this,[this]{pump();});
}
Link::~Link() {
    stop();
    if(uplinkSrc_)src_delete(uplinkSrc_);
}
void Link::detachBackend() {stop();backend_=nullptr;}

void Link::stop() {
    active_.store(false,std::memory_order_release);
    uplink_.store(int(Uplink::Off),std::memory_order_relaxed);
    pumpTimer_.stop();
    // abort() emits disconnected(), which clears client_: hold our own pointer.
    if(QTcpSocket* socket=client_.data()){client_.clear();socket->abort();socket->deleteLater();}
    if(server_){server_->close();server_->deleteLater();server_=nullptr;}
    if(programOpen_){program_.close();programOpen_=false;}
    input_.reset();
    if(backend_)backend_->junctionInputMainMix(false);
}

// ---- audio thread ----------------------------------------------------------

void Link::readInput(float* out,unsigned frames) noexcept {
    if(!active_.load(std::memory_order_acquire)){std::fill_n(out,size_t(frames)*2,0.f);return;}
    input_.read(out,frames);
}
void Link::capture(const float* full,const float* local,unsigned frames) noexcept {
    if(!active_.load(std::memory_order_acquire))return;
    const auto mode=Uplink(uplink_.load(std::memory_order_relaxed));
    const float* pcm=mode==Uplink::Full?full:mode==Uplink::Local?local:nullptr;
    // Always advance the frame counter so the WebView sees a gap, not a lie.
    unsigned offset=0;
    while(offset<frames){
        const unsigned count=std::min(frames-offset,4096u);
        if(pcm){
            PcmBlockInfo info;
            info.mediaFrame=captureFrame_;info.sourceFrame=captureFrame_;info.frameCount=count;
            info.sampleRateHz=kEngineRate;info.channels=2;info.epoch=1;info.generation=generation_;
            info.sequence=captureFrame_;
            uplinkRing_.push(pcm+size_t(offset)*2,info);
        }
        captureFrame_+=count;offset+=count;
    }
}

// ---- main thread: control ---------------------------------------------------

QJsonObject Link::command(const QString& op,const QJsonObject& p,QString* error) {
    auto fail=[&](const QString& message){if(error)*error=message;return QJsonObject{};};
    if(op=="start"){
        if(!backend_||!backend_->available())return fail("音声エンジンの準備ができていません");
        if(!server_){
            server_=new QTcpServer(this);
            if(!server_->listen(QHostAddress::LocalHost,0)){delete server_;server_=nullptr;return fail("Junctionの音声経路を開けませんでした");}
            connect(server_,&QTcpServer::newConnection,this,[this]{accept();});
            token_=QUuid::createUuid().toString(QUuid::Id128)+QUuid::createUuid().toString(QUuid::Id128);
        }
        input_.reset();
        ++generation_;
        active_.store(true,std::memory_order_release);
        // In v2 the J deck is a normal mixer channel: always in the main mix,
        // behind its own level, EQ and crossfader assign.
        backend_->junctionInputMainMix(true);
        pumpTimer_.start();
        return state();
    }
    if(op=="stop"){stop();return state();}
    if(op=="state")return state();
    if(!active())return fail("Junctionが開始されていません");
    if(op=="uplink"){
        const auto mode=p["mode"].toString();
        if(mode!="full"&&mode!="local"&&mode!="off")return fail("送出モードが不正です");
        uplink_.store(int(mode=="full"?Uplink::Full:mode=="local"?Uplink::Local:Uplink::Off),std::memory_order_relaxed);
        return state();
    }
    if(op=="input.set"){
        if(!backend_)return fail("音声エンジンがありません");
        const auto failure=backend_->junctionInputSet(p);
        if(!failure.isEmpty())return fail(failure);
        return state();
    }
    if(op=="program.open"){
        const auto id=p["deviceId"].toString();
        bool ok=false;const int index=id.section(':',-1).toInt(&ok);
        if(!ok)return fail("配信先のデバイスを選んでください");
        // Timeline off (t0 = 0): the WebView already delays and splices the
        // program; this stage only follows its own fill (~400 ms, so a stalled
        // WebView worker never underruns the stream) to the device.
        program_.setTimeline(0,0,24000);
        QString failure;
        if(!program_.open(index,&failure)){programOpen_=false;return fail(failure.isEmpty()?"配信先のデバイスを開けませんでした":failure);}
        programOpen_=true;programDevice_=id;
        if(p.contains("gain"))program_.setGain(float(p["gain"].toDouble(1)));
        return state();
    }
    if(op=="program.close"){program_.close();programOpen_=false;programDevice_.clear();return state();}
    if(op=="program.gain"){program_.setGain(float(p["gain"].toDouble(1)));return state();}
    return fail("不明なJunction操作です: "+op);
}

QJsonObject Link::state() const {
    QJsonObject program{{"open",programOpen_},{"deviceId",programDevice_.isEmpty()?QJsonValue::Null:QJsonValue(programDevice_)}};
    if(programOpen_){
        program["peak"]=program_.peak();program["underruns"]=double(program_.underruns());
        program["latencyMs"]=program_.deviceLatencySeconds()*1000;program["driftPpm"]=program_.deviceDriftPpm();
    }
    const char* modes[]={"off","full","local"};
    return {{"active",active()},{"port",server_?int(server_->serverPort()):0},{"token",token_},
            {"connected",bool(client_)&&upgraded_},
            {"uplink",modes[std::clamp(uplink_.load(),0,2)]},
            {"input",QJsonObject{{"receiving",input_.receiving()},{"latencyMs",double(input_.latencyFrames48k())/48.0},
                                 {"underruns",double(input_.underruns())},{"realignments",double(input_.realignments())}}},
            {"deck",backend_?backend_->junctionInputState():QJsonObject{}},
            {"program",program},
            {"frames",QJsonObject{{"input",double(framesIn_)},{"program",double(framesProgram_)},{"uplink",double(framesUp_)},
                                  {"dropped",double(dropped_)},{"rejected",double(rejected_)}}}};
}

// ---- main thread: loopback WebSocket ----------------------------------------
// Minimal RFC 6455 server: one client, binary frames, ping/pong, close.

void Link::accept() {
    while(auto* socket=server_->nextPendingConnection()){
        if(QTcpSocket* old=client_.data()){client_.clear();old->abort();old->deleteLater();}
        client_=socket;upgraded_=false;inbound_.clear();fragment_.clear();
        socket->setSocketOption(QAbstractSocket::LowDelayOption,1);
        connect(socket,&QTcpSocket::readyRead,this,[this,socket]{if(client_==socket)readSocket();});
        connect(socket,&QTcpSocket::disconnected,this,[this,socket]{if(client_==socket){client_.clear();upgraded_=false;}socket->deleteLater();});
    }
}

void Link::readSocket() {
    inbound_+=client_->readAll();
    if(!upgraded_){
        const auto end=inbound_.indexOf("\r\n\r\n");
        if(end<0){if(inbound_.size()>16384)client_->abort();return;}
        const auto request=QString::fromLatin1(inbound_.left(end));
        inbound_.remove(0,end+4);
        const auto lines=request.split("\r\n");
        const auto target=lines.value(0).section(' ',1,1);
        QString key;
        for(const auto& line:lines){if(line.startsWith("Sec-WebSocket-Key:",Qt::CaseInsensitive))key=line.section(':',1).trimmed();}
        const QUrlQuery query(QUrl(target).query());
        if(key.isEmpty()||query.queryItemValue("token")!=token_||token_.isEmpty()){
            client_->write("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            client_->disconnectFromHost();return;
        }
        const auto accept=QCryptographicHash::hash((key+"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").toLatin1(),QCryptographicHash::Sha1).toBase64();
        client_->write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: "+accept+"\r\n\r\n");
        upgraded_=true;
        // A new connection is a new stream: never splice it onto the old one.
        input_.reset();++generation_;
    }
    while(client_&&inbound_.size()>=2){
        const auto* d=reinterpret_cast<const unsigned char*>(inbound_.constData());
        const bool fin=d[0]&0x80;const int opcode=d[0]&0x0f;const bool masked=d[1]&0x80;
        quint64 length=d[1]&0x7f;qsizetype offset=2;
        if(length==126){if(inbound_.size()<4)return;length=(quint64(d[2])<<8)|d[3];offset=4;}
        else if(length==127){if(inbound_.size()<10)return;length=0;for(int i=0;i<8;++i)length=(length<<8)|d[2+i];offset=10;}
        if(!masked||length>quint64(kMaxMessage)){client_->abort();return;}
        if(inbound_.size()<offset+4+qsizetype(length))return;
        const auto* mask=d+offset;offset+=4;
        QByteArray payload(qsizetype(length),Qt::Uninitialized);
        for(quint64 i=0;i<length;++i)payload[qsizetype(i)]=char(d[offset+qsizetype(i)]^mask[i%4]);
        inbound_.remove(0,offset+qsizetype(length));
        if(opcode==0){fragment_+=payload;if(fin){handleFrame(fragmentOpcode_,fragment_);fragment_.clear();}continue;}
        if(!fin&&opcode<8){fragmentOpcode_=opcode;fragment_=payload;continue;}
        handleFrame(opcode,payload);
    }
}

void Link::handleFrame(int opcode,const QByteArray& payload) {
    if(opcode==0x2){handleAudio(payload);return;}
    if(opcode==0x9){sendFrame(0xA,payload.constData(),payload.size());return;}
    if(opcode==0x8){sendFrame(0x8,payload.constData(),std::min<qsizetype>(payload.size(),2));if(client_)client_->disconnectFromHost();return;}
}

void Link::handleAudio(const QByteArray& payload) {
    if(!active()||payload.size()<kHeaderBytes){++rejected_;return;}
    const auto* p=payload.constData();
    const quint8 kind=quint8(p[0]),channels=quint8(p[1]);
    const auto frames=readLe<quint32>(p+4);
    const auto first=readLe<double>(p+8);
    if(channels!=2||frames==0||frames>16384||payload.size()!=kHeaderBytes+qsizetype(frames)*8||!std::isfinite(first)||first<0){++rejected_;return;}
    const auto* pcm=reinterpret_cast<const float*>(p+kHeaderBytes);
    PcmBlockInfo info;
    info.epoch=1;info.generation=generation_;info.mediaFrame=quint64(first);info.sourceFrame=info.mediaFrame;
    info.sampleRateHz=48000;info.channels=2;info.frameCount=frames;
    if(kind==kKindInput){
        info.sequence=inputSeq_++;
        input_.write(pcm,info);
        framesIn_+=frames;
    }else if(kind==kKindProgram){
        if(!programOpen_)return;
        for(quint32 offset=0;offset<frames;){
            const auto count=std::min<quint32>(frames-offset,4096);
            auto block=info;block.frameCount=count;block.mediaFrame=info.mediaFrame+offset;block.sequence=programSeq_++;
            if(!program_.inputRing().push(pcm+size_t(offset)*2,block))++dropped_;
            offset+=count;
        }
        framesProgram_+=frames;
    }else ++rejected_;
}

void Link::sendFrame(int opcode,const char* data,qsizetype size) {
    if(!client_||!upgraded_)return;
    QByteArray header;
    header.append(char(0x80|opcode));
    if(size<126)header.append(char(size));
    else if(size<65536){header.append(char(126));header.append(char((size>>8)&0xff));header.append(char(size&0xff));}
    else{header.append(char(127));for(int i=7;i>=0;--i)header.append(char((quint64(size)>>(8*i))&0xff));}
    client_->write(header);
    if(size)client_->write(data,size);
}

void Link::pump() {
    // Drain the uplink ring, convert 44.1 -> 48 kHz and send. Frames captured
    // while the socket is not ready are dropped, never buffered.
    const bool ready=client_&&upgraded_&&client_->bytesToWrite()<kMaxBacklogBytes;
    while(true){
        auto popped=uplinkRing_.popBlock(uplinkIn_.data(),4096);
        if(!popped.frames)break;
        if(!ready||!uplinkSrc_){++dropped_;continue;}
        SRC_DATA data{};
        data.data_in=uplinkIn_.data();data.input_frames=long(popped.frames);
        data.data_out=uplinkOut_.data();data.output_frames=long(uplinkOut_.size()/2);
        data.src_ratio=48000.0/kEngineRate;
        if(src_process(uplinkSrc_,&data)||data.output_frames_gen<=0)continue;
        const auto frames=quint32(data.output_frames_gen);
        QByteArray message(kHeaderBytes+qsizetype(frames)*8,Qt::Uninitialized);
        auto* m=message.data();
        m[0]=char(kKindUplink);m[1]=2;m[2]=0;m[3]=0;
        std::memcpy(m+4,&frames,4);std::memcpy(m+8,&uplinkFrame_,8);
        std::memcpy(m+kHeaderBytes,uplinkOut_.data(),size_t(frames)*8);
        uplinkFrame_+=frames;framesUp_+=frames;
        sendFrame(0x2,message.constData(),message.size());
    }
}
}
