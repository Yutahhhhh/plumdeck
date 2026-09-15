#include "media_transport.h"
#include <deque>
#include "audio_clock.h"
#include <QDir>
#include <QDebug>
#include <QFile>
#include <QRegularExpression>
#include <QtEndian>
#include <thread>
#include <mutex>
#include <map>
#include <array>
#include <chrono>
#include <cstring>
#include <samplerate.h>
#include <opus/opus.h>
#include <openssl/evp.h>
#include <openssl/pem.h>
#include <openssl/x509.h>
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
#include <rtc/rtc.hpp>
#endif
namespace junction {
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
class FeedbackSession final : public rtc::RtcpReceivingSession {
public:
 std::function<void(quint16)> retry;std::function<void()> reportReceived;
 void incoming(rtc::message_vector& messages,const rtc::message_callback& send) override {
  for(const auto& m:messages){const auto*p=reinterpret_cast<const uchar*>(m->data());size_t remaining=m->size();while(remaining>=4){size_t n=(qFromBigEndian<quint16>(p+2)+1)*4;if(n>remaining||n<4)break;if(p[1]==200&&reportReceived)reportReceived();if(p[1]==205&&(p[0]&31)==1&&n>=16&&retry){for(size_t i=12;i+4<=n;i+=4){auto seq=qFromBigEndian<quint16>(p+i);auto bits=qFromBigEndian<quint16>(p+i+2);retry(seq);for(int j=0;j<16;j++)if(bits&(1<<j))retry(quint16(seq+j+1));}}p+=n;remaining-=n;}}
  rtc::RtcpReceivingSession::incoming(messages,send);
 }
};
#endif

namespace {
QString normalize(QString s){return s.remove(':').toLower();}
bool fail(QString* e,QString s){if(e)*e=s;return false;}
void ensureNetworkRuntime() {
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 // libnice owns a process-wide GLib thread. Join it before static destruction,
 // after all scoped peer connections and signaling sockets have closed.
 struct NetworkLifetime {
  NetworkLifetime(){rtc::Preload();}
  ~NetworkLifetime(){rtc::Cleanup().wait();}
 };
 static NetworkLifetime lifetime;
#endif
}
}
struct MediaTransport::Impl {
 bool forceRelay=false,lite=false,liteHost=false,browserReceive=false;QString peer;QStringList ice;Callbacks cb;std::shared_ptr<Identity> identity;
 PcmRing decoded{128,960,2},preCodec{64,960,2};PcmRing* source=nullptr;std::atomic<bool> running{false};std::thread worker;std::mutex mutex;
 struct SourceBlock {PcmBlockInfo info;std::vector<float> samples;};
 std::deque<SourceBlock> sourceHistory,replaySource;unsigned historyFrames=0;
 qint64 receiveWindow[3]={},bulkWindow=0,validationWindow=0;int receiveCount[3]={},bulkBytes=0,validationBytes=0;
 std::atomic<quint64> receivedPackets{0},sentPackets{0},senderReports{0},receivedReports{0},nacksSent{0},retransmittedPackets{0};
 quint64 minimumEpoch=0,minimumGeneration=0;StreamManifest tx,rx;bool hasTx=false,hasRx=false,automaticManifest=false,waitingManifestAck=false; qint64 manifestSentAt=0;quint16 sequence=0;quint64 decodedSeq=0,nextRx=0;bool rxStarted=false;quint32 browserTimestamp=0,browserSsrc=0;int browserPayloadType=-1;
 struct Packet{QByteArray bytes;qint64 arrived;int retries=0;};std::map<quint64,Packet> packets;
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 std::shared_ptr<rtc::PeerConnection> pc[2];std::shared_ptr<rtc::DataChannel> channels[3];std::shared_ptr<rtc::Track> track;
 // Written from libdatachannel's threads, read from the session thread.
 std::atomic<bool> gathered[2]{{false},{false}};std::atomic<int> link[2]{{int(LinkState::New)},{int(LinkState::New)}};
 std::shared_ptr<rtc::RtpPacketizationConfig> reportConfig;std::shared_ptr<rtc::RtcpSrReporter> reporter;
 std::map<quint16,Packet> resend;quint16 lastReceivedSequence=0;quint64 lastReceivedFrame=0;bool receivedSequence=false;qint64 lastNack=0;
 void channel(std::shared_ptr<rtc::DataChannel> c,int kind){if(c->label()!=(kind==2?"validation":kind==1?"junction.bulk":"junction.control")){c->close();return;} {std::lock_guard lock(mutex);channels[kind]=c;}c->onMessage([this,kind](rtc::message_variant v){QByteArray b;if(auto p=std::get_if<rtc::binary>(&v))b=QByteArray(reinterpret_cast<const char*>(p->data()),p->size());else b=QByteArray::fromStdString(std::get<std::string>(v));if(b.size()>(kind?kMaxChunkBytes+72:kMaxControlMessageBytes))return;{std::lock_guard lock(mutex);auto now=monotonicNanos();if(now-receiveWindow[kind]>=1000000000){receiveWindow[kind]=now;receiveCount[kind]=0;}if(++receiveCount[kind]>(kind==2?32:256))return;}auto& f=kind==2?cb.validation:kind==1?cb.bulk:cb.control;if(f)f(b);});}
 void attach(std::shared_ptr<rtc::Track> t){std::lock_guard attachLock(mutex);track=t;if(lite&&!liteHost&&hasTx){auto media=t->description();media.addSSRC(tx.ssrc,"junction");t->setDescription(media);}auto feedback=std::make_shared<FeedbackSession>();feedback->reportReceived=[this]{++receivedReports;};feedback->retry=[this](quint16 seq){std::lock_guard lock(mutex);auto i=resend.find(seq);if(i!=resend.end()&&i->second.retries<2&&monotonicNanos()-i->second.arrived<150000000&&track&&track->isOpen()){try{++i->second.retries;track->send(reinterpret_cast<const std::byte*>(i->second.bytes.constData()),i->second.bytes.size());++retransmittedPackets;}catch(...){}}};reportConfig=std::make_shared<rtc::RtpPacketizationConfig>(hasTx?tx.ssrc:1,"junction",111,48000);reporter=std::make_shared<rtc::RtcpSrReporter>(reportConfig);feedback->addToChain(reporter);t->setMediaHandler(feedback);t->onMessage([this](rtc::message_variant v){auto b=std::get_if<rtc::binary>(&v);if(!b||b->size()<12||b->size()>1500)return;auto p=reinterpret_cast<const uchar*>(b->data());if((p[0]>>6)!=2)return;const int payloadType=p[1]&127;size_t header=12+size_t(p[0]&15)*4;if(header>b->size())return;if(p[0]&0x10){if(header+4>b->size())return;const size_t extension=4+size_t(qFromBigEndian<quint16>(p+header+2))*4;if(header+extension>b->size())return;header+=extension;}size_t payloadSize=b->size()-header;if(p[0]&0x20){const auto padding=size_t(p[b->size()-1]);if(!padding||padding>payloadSize)return;payloadSize-=padding;}if(!payloadSize)return;std::lock_guard lock(mutex);if(!hasRx)return;const auto ssrc=qFromBigEndian<quint32>(p+8);const auto ts=qFromBigEndian<quint32>(p+4);if(browserReceive){if(browserPayloadType<0)browserPayloadType=payloadType;if(payloadType!=browserPayloadType)return;if(!browserSsrc){browserSsrc=ssrc;browserTimestamp=ts;rx.ssrc=ssrc;rx.rtpTimestampOrigin=ts;}if(ssrc!=browserSsrc)return;}else if(payloadType!=111||ssrc!=rx.ssrc)return;quint64 reference=rxStarted?nextRx:rx.mediaFrameOrigin;quint32 expected=rx.rtpTimestampOrigin+quint32(reference-rx.mediaFrameOrigin);qint64 delta=qint32(ts-expected);if(delta < -qint64(24000)||delta>qint64(96000)|| (delta<0&&quint64(-delta)>reference))return;quint64 frame=delta<0?reference-quint64(-delta):reference+quint64(delta);if(frame<rx.mediaFrameOrigin||(frame-rx.mediaFrameOrigin)%960||packets.size()>=100)return;++receivedPackets;auto seq=qFromBigEndian<quint16>(p+2);if(!receivedSequence||frame>lastReceivedFrame){lastReceivedFrame=frame;lastReceivedSequence=seq;receivedSequence=true;}packets.emplace(frame,Packet{QByteArray(reinterpret_cast<const char*>(p+header),int(payloadSize)),monotonicNanos()});});}
#endif
 void run(){int e=0;OpusEncoder* enc=opus_encoder_create(48000,2,OPUS_APPLICATION_AUDIO,&e);OpusDecoder* dec=opus_decoder_create(48000,2,&e);SRC_STATE* src=src_new(SRC_SINC_FASTEST,2,&e);if(!enc||!dec||!src){if(cb.error)cb.error("Cannot initialize media codec");if(enc)opus_encoder_destroy(enc);if(dec)opus_decoder_destroy(dec);if(src)src_delete(src);return;}opus_encoder_ctl(enc,OPUS_SET_BITRATE(320000));opus_encoder_ctl(enc,OPUS_SET_VBR(0));opus_encoder_ctl(enc,OPUS_SET_DTX(0));
 std::array<float,8192> input{},converted{};std::array<float,1920> frame{},out{};std::array<unsigned char,1400> encoded{};int fill=0;quint64 anchor=0,sourceGeneration=~quint64(0),sourceEpoch=~quint64(0);quint32 rate=0;int conceal=0;PopResult held;bool holding=false;quint64 sourceEnd=0;
 while(running.load()){
  PcmRing* ring;{std::lock_guard lock(mutex);ring=source;}
  if(ring){auto got=holding?held:[&]{
      PopResult next;
      {std::lock_guard lock(mutex);if(!replaySource.empty()){auto block=std::move(replaySource.front());replaySource.pop_front();next.info=block.info;next.frames=block.info.frameCount;std::copy(block.samples.begin(),block.samples.end(),input.begin());}}
      if(!next.frames)next=ring->popBlock(input.data(),4096);
      if(next.frames){std::lock_guard lock(mutex);sourceHistory.push_back({next.info,std::vector<float>(input.data(),input.data()+next.frames*2)});historyFrames+=next.frames;while(historyFrames>16384&&sourceHistory.size()>1){historyFrames-=sourceHistory.front().info.frameCount;sourceHistory.pop_front();}}
      return next;
   }();if(got.frames){
   bool automatic,stale;{std::lock_guard lock(mutex);automatic=automaticManifest;stale=automatic&&(got.info.epoch<minimumEpoch||(got.info.epoch==minimumEpoch&&got.info.generation<minimumGeneration));}
   if(stale){holding=false;held={};sourceGeneration=~quint64(0);continue;}
   if(!holding&&(got.info.generation!=sourceGeneration||got.info.epoch!=sourceEpoch||got.info.sampleRateHz!=rate||(automatic&&got.info.sourceFrame!=sourceEnd))){
    src_reset(src);opus_encoder_ctl(enc,OPUS_RESET_STATE);fill=0;anchor=got.info.mediaFrame;sourceGeneration=got.info.generation;sourceEpoch=got.info.epoch;rate=got.info.sampleRateHz;
    if(automatic){StreamManifest manifest;{std::lock_guard lock(mutex);manifest=tx;}manifest.streamId=secureRandomHex(16);manifest.epoch=sourceEpoch;manifest.generation=sourceGeneration;manifest.mediaFrameOrigin=anchor;auto random=secureRandomBytes(8);manifest.ssrc=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()));if(!manifest.ssrc)manifest.ssrc=1;manifest.rtpTimestampOrigin=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()+4));
     {std::lock_guard lock(mutex);tx=manifest;hasTx=true;waitingManifestAck=true;manifestSentAt=monotonicNanos();
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
      resend.clear();
#endif
     }
     held=got;holding=true;if(cb.producerManifest)cb.producerManifest(manifest);
    }
   }
   bool waiting=false,timedOut=false;{std::lock_guard lock(mutex);waiting=automaticManifest&&waitingManifestAck;if(waiting&&monotonicNanos()-manifestSentAt>2000000000){waitingManifestAck=false;hasTx=false;timedOut=true;}}
   if(timedOut){holding=false;held={};sourceGeneration=~quint64(0);if(cb.error)cb.error("Producer manifest acknowledgment timed out");}
   else if(!waiting){holding=false;held={};sourceEnd=got.info.sourceFrame+got.frames;
   long used=0;while(used<got.frames&&rate){SRC_DATA data{};data.data_in=input.data()+used*2;data.input_frames=got.frames-used;data.data_out=converted.data();data.output_frames=4096;data.src_ratio=48000.0/rate;if(src_process(src,&data))break;used+=data.input_frames_used;
    for(long i=0;i<data.output_frames_gen;i++){frame[fill*2]=converted[i*2];frame[fill*2+1]=converted[i*2+1];if(++fill==960){PcmBlockInfo raw;raw.epoch=sourceEpoch;raw.generation=sourceGeneration;raw.mediaFrame=anchor;raw.sourceFrame=anchor;raw.sequence=anchor/960;raw.frameCount=960;raw.sampleRateHz=48000;raw.channels=2;preCodec.push(frame.data(),raw);int n=opus_encode_float(enc,frame.data(),960,encoded.data(),encoded.size());std::lock_guard lock(mutex);
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
    if(n>0&&hasTx&&track&&track->isOpen()&&tx.epoch==sourceEpoch&&tx.generation==sourceGeneration&&anchor>=tx.mediaFrameOrigin){QByteArray packet(12+n,0);auto p=reinterpret_cast<uchar*>(packet.data());p[0]=0x80;p[1]=111;qToBigEndian<quint16>(sequence++,p+2);qToBigEndian<quint32>(tx.rtpTimestampOrigin+quint32(anchor-tx.mediaFrameOrigin),p+4);qToBigEndian<quint32>(tx.ssrc,p+8);memcpy(p+12,encoded.data(),n);resend[quint16(sequence-1)]={packet,monotonicNanos()};for(auto it=resend.begin();it!=resend.end();)if(monotonicNanos()-it->second.arrived>150000000)it=resend.erase(it);else ++it;reportConfig->ssrc=tx.ssrc;reportConfig->timestamp=tx.rtpTimestampOrigin+quint32(anchor-tx.mediaFrameOrigin);if(sequence%50==0){reporter->setNeedsToReport();++senderReports;}try{if(track->send(reinterpret_cast<const std::byte*>(packet.constData()),packet.size()))++sentPackets;}catch(const std::exception&){} }
#endif
    fill=0;anchor+=960;}}
    if(!data.input_frames_used&&!data.output_frames_gen)break;
   }
  }}}
  {std::lock_guard lock(mutex);if(hasRx&&!packets.empty()){
   if(!rxStarted){nextRx=packets.begin()->first;rxStarted=true;opus_decoder_ctl(dec,OPUS_RESET_STATE);conceal=0;}
   while(!packets.empty()&&packets.begin()->first<nextRx)packets.erase(packets.begin());
   auto it=packets.find(nextRx);
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
   if(it==packets.end()&&!packets.empty()&&receivedSequence&&lastReceivedFrame>nextRx&&monotonicNanos()-lastNack>20000000&&track&&track->isOpen()){QByteArray nack(16,0);auto*p=reinterpret_cast<uchar*>(nack.data());p[0]=0x81;p[1]=205;qToBigEndian<quint16>(3,p+2);qToBigEndian<quint32>(rx.ssrc,p+8);qToBigEndian<quint16>(quint16(lastReceivedSequence-(lastReceivedFrame-nextRx)/960),p+12);try{track->send(reinterpret_cast<const std::byte*>(nack.constData()),nack.size());++nacksSent;}catch(...){}lastNack=monotonicNanos();}
#endif
   bool ready=it!=packets.end()&&monotonicNanos()-it->second.arrived>=60000000;bool lost=it==packets.end()&&!packets.empty()&&monotonicNanos()-packets.begin()->second.arrived>=60000000;
   if(ready||lost){int n=0;if(ready){n=opus_decode_float(dec,reinterpret_cast<const unsigned char*>(it->second.bytes.constData()),it->second.bytes.size(),out.data(),960,0);packets.erase(it);conceal=0;}else if(++conceal<=3)n=opus_decode_float(dec,nullptr,0,out.data(),960,0);if(n!=960)out.fill(0);PcmBlockInfo info;info.epoch=rx.epoch;info.generation=rx.generation;quint32 trim=nextRx==rx.mediaFrameOrigin?rx.codecLookaheadFrames:0;info.mediaFrame=nextRx+trim-rx.codecLookaheadFrames;info.sequence=decodedSeq++;info.sourceFrame=info.mediaFrame;info.frameCount=960-trim;info.sampleRateHz=48000;info.channels=2;decoded.push(out.data()+trim*2,info);nextRx+=960;}
  }}
  std::this_thread::sleep_for(std::chrono::milliseconds(1));
 }
 src_delete(src);opus_encoder_destroy(enc);opus_decoder_destroy(dec);
 }
};
std::shared_ptr<MediaTransport::Identity> MediaTransport::createIdentity(const QString& directory,QString* error){
 QDir().mkpath(directory);auto id=std::make_shared<Identity>();id->certificatePath=directory+"/"+secureRandomHex(12)+".crt";id->keyPath=directory+"/"+secureRandomHex(12)+".key";
 EVP_PKEY_CTX* ctx=EVP_PKEY_CTX_new_id(EVP_PKEY_EC,nullptr);EVP_PKEY* key=nullptr;X509* cert=X509_new();bool ok=ctx&&cert&&EVP_PKEY_keygen_init(ctx)>0&&EVP_PKEY_CTX_set_ec_paramgen_curve_nid(ctx,NID_X9_62_prime256v1)>0&&EVP_PKEY_keygen(ctx,&key)>0;
 if(ok){X509_set_version(cert,2);ASN1_INTEGER_set(X509_get_serialNumber(cert),1);X509_gmtime_adj(X509_get_notBefore(cert),-60);X509_gmtime_adj(X509_get_notAfter(cert),86400*7);X509_set_pubkey(cert,key);auto name=X509_get_subject_name(cert);X509_NAME_add_entry_by_txt(name,"CN",MBSTRING_ASC,reinterpret_cast<const unsigned char*>("Junction"),-1,-1,0);X509_set_issuer_name(cert,name);ok=X509_sign(cert,key,EVP_sha256())>0;}
 BIO* cb=BIO_new(BIO_s_mem());BIO* kb=BIO_new(BIO_s_mem());if(ok)ok=PEM_write_bio_X509(cb,cert)&&PEM_write_bio_PrivateKey(kb,key,nullptr,nullptr,0,nullptr,nullptr);
 if(ok){unsigned char digest[EVP_MAX_MD_SIZE];unsigned int n=0;ok=X509_digest(cert,EVP_sha256(),digest,&n);id->fingerprint=QString::fromLatin1(QByteArray(reinterpret_cast<char*>(digest),n).toHex());for(auto pair:{std::make_pair(id->certificatePath,cb),std::make_pair(id->keyPath,kb)}){char* p;auto size=BIO_get_mem_data(pair.second,&p);QFile f(pair.first);ok=ok&&f.open(QIODevice::WriteOnly|QIODevice::NewOnly);if(ok){f.setPermissions(QFile::ReadOwner|QFile::WriteOwner);ok=f.write(p,size)==size;}}}
 BIO_free(cb);BIO_free(kb);X509_free(cert);EVP_PKEY_free(key);EVP_PKEY_CTX_free(ctx);if(!ok){QFile::remove(id->certificatePath);QFile::remove(id->keyPath);fail(error,"DTLS identity creation failed");return{};}return id;
}
MediaTransport::MediaTransport(QString peer,QStringList ice,Callbacks cb,std::shared_ptr<Identity> identity,bool forceRelay):d(new Impl){ensureNetworkRuntime();d->peer=peer;d->ice=ice;d->cb=std::move(cb);d->identity=std::move(identity);d->forceRelay=forceRelay;}
MediaTransport::~MediaTransport(){close();}
bool MediaTransport::available(){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 ensureNetworkRuntime();
 return true;
#else
 return false;
#endif
}
bool MediaTransport::start(bool offerer,QString* error){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 if(d->running)return fail(error,"Transport already started");try{rtc::Configuration cfg;if(d->forceRelay)cfg.iceTransportPolicy=rtc::TransportPolicy::Relay;cfg.disableAutoNegotiation=true;cfg.maxMessageSize=65536;for(const auto& s:d->ice)cfg.iceServers.emplace_back(s.toStdString());if(d->identity){cfg.certificatePemFile=d->identity->certificatePath.toStdString();cfg.keyPemFile=d->identity->keyPath.toStdString();}
 for(int i=0;i<2;i++){d->pc[i]=std::make_shared<rtc::PeerConnection>(cfg);const bool trace=qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE");if(trace)d->pc[i]->onIceStateChange([i](auto state){qInfo()<<"junction ice state"<<i<<int(state);});
  d->pc[i]->onStateChange([this,i,trace](rtc::PeerConnection::State state){if(trace)qInfo()<<"junction transport state"<<i<<int(state);
   const auto mapped=state==rtc::PeerConnection::State::Connecting?LinkState::Connecting:state==rtc::PeerConnection::State::Connected?LinkState::Connected:state==rtc::PeerConnection::State::Disconnected?LinkState::Disconnected:state==rtc::PeerConnection::State::Failed?LinkState::Failed:state==rtc::PeerConnection::State::Closed?LinkState::Closed:LinkState::New;
   d->link[i].store(int(mapped),std::memory_order_release);if(d->cb.linkState)d->cb.linkState(i!=0,mapped);});
  // Non-trickle: the aggregated local description is only complete here.
  d->pc[i]->onGatheringStateChange([this,i,trace](rtc::PeerConnection::GatheringState state){if(trace)qInfo()<<"junction gathering state"<<i<<int(state);
   if(state!=rtc::PeerConnection::GatheringState::Complete)return;
   d->gathered[i].store(true,std::memory_order_release);if(d->cb.gatheringComplete)d->cb.gatheringComplete(i!=0);});
  d->pc[i]->onLocalDescription([this,i](rtc::Description s){QString fp;auto f=s.fingerprint();if(f)fp=QString::fromStdString(f->value);if(d->cb.localDescription)d->cb.localDescription(i,QString::fromStdString(std::string(s)),QString::fromStdString(s.typeString()),normalize(fp));});d->pc[i]->onLocalCandidate([this,i](rtc::Candidate c){if(d->cb.localCandidate)d->cb.localCandidate(i,QString::fromStdString(std::string(c)),QString::fromStdString(c.mid()));});d->pc[i]->onDataChannel([this,i](auto c){d->channel(c,i==0&&c->label()=="validation"?2:i);});if(i==0)d->pc[i]->onTrack([this](auto t){d->attach(t);});if(offerer)d->channel(d->pc[i]->createDataChannel(i?"junction.bulk":"junction.control"),i);}
 if(offerer){d->channel(d->pc[0]->createDataChannel("validation"),2);rtc::Description::Audio audio("junction-audio",rtc::Description::Direction::SendRecv);audio.addOpusCodec(111,"minptime=20;maxptime=20;stereo=1;sprop-stereo=1;maxaveragebitrate=320000");if(d->hasTx)audio.addSSRC(d->tx.ssrc,"junction");d->attach(d->pc[0]->addTrack(audio));for(auto& pc:d->pc)pc->setLocalDescription();}d->running=true;d->worker=std::thread([this]{d->run();});return true;
 }catch(const std::exception&){close();return fail(error,"WebRTC initialization failed");}
#else
 return fail(error,"WebRTC unavailable");
#endif
}
bool MediaTransport::startLite(bool host,QString* error){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 if(d->running)return fail(error,"Transport already started");d->lite=true;d->liteHost=host;try{rtc::Configuration cfg;if(d->forceRelay)cfg.iceTransportPolicy=rtc::TransportPolicy::Relay;cfg.disableAutoNegotiation=true;cfg.maxMessageSize=65536;for(const auto&s:d->ice)cfg.iceServers.emplace_back(s.toStdString());if(d->identity){cfg.certificatePemFile=d->identity->certificatePath.toStdString();cfg.keyPemFile=d->identity->keyPath.toStdString();}d->pc[0]=std::make_shared<rtc::PeerConnection>(cfg);const bool trace=qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE");d->pc[0]->onStateChange([this,trace](rtc::PeerConnection::State state){if(trace)qInfo()<<"junction lite transport state"<<int(state);const auto mapped=state==rtc::PeerConnection::State::Connecting?LinkState::Connecting:state==rtc::PeerConnection::State::Connected?LinkState::Connected:state==rtc::PeerConnection::State::Disconnected?LinkState::Disconnected:state==rtc::PeerConnection::State::Failed?LinkState::Failed:state==rtc::PeerConnection::State::Closed?LinkState::Closed:LinkState::New;d->link[0].store(int(mapped),std::memory_order_release);if(d->cb.linkState)d->cb.linkState(false,mapped);});d->pc[0]->onGatheringStateChange([this](rtc::PeerConnection::GatheringState state){if(state!=rtc::PeerConnection::GatheringState::Complete)return;d->gathered[0].store(true,std::memory_order_release);if(d->cb.gatheringComplete)d->cb.gatheringComplete(false);});d->pc[0]->onLocalDescription([this](rtc::Description s){QString fp;auto f=s.fingerprint();if(f)fp=QString::fromStdString(f->value);if(d->cb.localDescription)d->cb.localDescription(false,QString::fromStdString(std::string(s)),QString::fromStdString(s.typeString()),normalize(fp));});d->pc[0]->onDataChannel([this](auto c){d->channel(c,0);});d->pc[0]->onTrack([this](auto t){d->attach(t);});if(host){d->channel(d->pc[0]->createDataChannel("junction.control"),0);rtc::Description::Audio audio("0",rtc::Description::Direction::RecvOnly);audio.addOpusCodec(111,"minptime=20;stereo=1;sprop-stereo=1");d->attach(d->pc[0]->addTrack(audio));d->pc[0]->setLocalDescription();}d->running=true;d->worker=std::thread([this]{d->run();});return true;}catch(const std::exception&){close();return fail(error,"WebRTC initialization failed");}
#else
 Q_UNUSED(host);return fail(error,"WebRTC unavailable");
#endif
}
void MediaTransport::close(){d->running=false;if(d->worker.joinable())d->worker.join();
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 for(auto& pc:d->pc)if(pc){pc->resetCallbacks();pc->close();}for(auto& c:d->channels)if(c){c->resetCallbacks();c->close();c.reset();}if(d->track){d->track->resetCallbacks();d->track.reset();}for(auto&pc:d->pc)pc.reset();
#endif
}
bool MediaTransport::remoteDescription(bool bulk,const QString&sdp,const QString&type,const QString&fp,QString*error){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 if(!d->pc[bulk]||normalize(fp).size()!=64||sdp.size()>65536)return fail(error,"Invalid authenticated SDP");for(const auto& line:sdp.split('\n')){if(line.startsWith("a=fingerprint:")){auto v=line.trimmed().mid(14).split(' ');if(v.size()!=2||v[0]!="sha-256"||normalize(v[1])!=normalize(fp))return fail(error,"DTLS media identity mismatch");}}try{rtc::Description desc(sdp.toStdString(),type.toStdString());auto f=desc.fingerprint();if(!f||normalize(QString::fromStdString(f->value))!=normalize(fp))return fail(error,"DTLS identity mismatch");d->pc[bulk]->setRemoteDescription(desc);if(type=="offer")d->pc[bulk]->setLocalDescription();return true;}catch(const std::exception&){return fail(error,"Invalid remote SDP");}
#else
 return fail(error,"WebRTC unavailable");
#endif
}
bool MediaTransport::remoteCandidate(bool bulk,const QString&c,const QString&mid){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 if(!d->pc[bulk]||c.size()>4096||mid.size()>128)return false;try{d->pc[bulk]->addRemoteCandidate(rtc::Candidate(c.toStdString(),mid.toStdString()));return true;}catch(...){return false;}
#else
 return false;
#endif
}
bool MediaTransport::sendControl(const QByteArray&b){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 std::shared_ptr<rtc::DataChannel> c;{std::lock_guard lock(d->mutex);c=d->channels[0];}if(!c||!c->isOpen()||b.size()>65536||c->bufferedAmount()>131072)return false;try{return c->send(reinterpret_cast<const std::byte*>(b.constData()),b.size());}catch(...){return false;}
#else
 return false;
#endif
}
bool MediaTransport::sendBulk(const QByteArray&b){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 std::shared_ptr<rtc::DataChannel> c;{std::lock_guard lock(d->mutex);c=d->channels[1];}if(!c||!c->isOpen()||b.size()>kMaxChunkBytes+72||c->bufferedAmount()>65536)return false;{std::lock_guard lock(d->mutex);auto now=monotonicNanos();if(now-d->bulkWindow>=1000000000){d->bulkWindow=now;d->bulkBytes=0;}if(d->bulkBytes+b.size()>4*1024*1024)return false;d->bulkBytes+=b.size();}try{return c->send(reinterpret_cast<const std::byte*>(b.constData()),b.size());}catch(...){return false;}
#else
 return false;
#endif
}
bool MediaTransport::sendKeepAlive(){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 std::shared_ptr<rtc::DataChannel> c;{std::lock_guard lock(d->mutex);c=d->channels[1];}
 if(!c||!c->isOpen()||c->bufferedAmount()>65536)return false;
 const std::byte marker{0};try{return c->send(&marker,1);}catch(...){return false;}
#else
 return false;
#endif
}
bool MediaTransport::sendValidation(const QByteArray&b){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 std::shared_ptr<rtc::DataChannel> c,control;{std::lock_guard lock(d->mutex);c=d->channels[2];control=d->channels[0];}if(!c||!c->isOpen()||b.size()>kMaxChunkBytes+72||c->bufferedAmount()>32768||(control&&control->bufferedAmount()>0))return false;{std::lock_guard lock(d->mutex);auto now=monotonicNanos();if(now-d->validationWindow>=1000000000){d->validationWindow=now;d->validationBytes=0;}if(d->validationBytes+b.size()>256*1024)return false;d->validationBytes+=b.size();}try{return c->send(reinterpret_cast<const std::byte*>(b.constData()),b.size());}catch(...){return false;}
#else
 return false;
#endif
}
bool MediaTransport::setSendManifest(const StreamManifest&m){if(m.streamId.isEmpty()||m.producerPeerId.isEmpty()||!m.ssrc||m.codecLookaheadFrames>=960)return false;std::lock_guard lock(d->mutex);d->tx=m;d->hasTx=true;d->minimumEpoch=m.epoch;d->minimumGeneration=m.generation;d->waitingManifestAck=false;return true;}
bool MediaTransport::setReceiveManifest(const StreamManifest&m){if(m.streamId.isEmpty()||m.producerPeerId!=d->peer||!m.ssrc||m.codecLookaheadFrames>=960)return false;std::lock_guard lock(d->mutex);d->rx=m;d->hasRx=true;d->packets.clear();d->rxStarted=false;
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 d->receivedSequence=false;
#endif
 return true;}
void MediaTransport::setBrowserReceive(quint64 epoch,quint64 mediaFrameOrigin){std::lock_guard lock(d->mutex);d->browserReceive=true;d->browserPayloadType=-1;d->browserTimestamp=0;d->browserSsrc=0;d->rx=StreamManifest{QStringLiteral("browser"),d->peer,epoch,1,mediaFrameOrigin,1,0,0};d->hasRx=true;d->packets.clear();d->rxStarted=false;
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 d->receivedSequence=false;
#endif
}
void MediaTransport::setBrowserSendEpoch(quint64 epoch,quint64 mediaFrameOrigin){std::lock_guard lock(d->mutex);if(!d->hasTx)return;d->tx.epoch=epoch;++d->tx.generation;d->tx.mediaFrameOrigin=mediaFrameOrigin;d->tx.rtpTimestampOrigin=quint32(mediaFrameOrigin);d->minimumEpoch=epoch;d->minimumGeneration=d->tx.generation;d->waitingManifestAck=false;}
void MediaTransport::startProducer(PcmRing*r){std::lock_guard lock(d->mutex);d->source=r;}
MediaTransport::Statistics MediaTransport::statistics()const{return {d->receivedPackets.load(),d->sentPackets.load(),d->senderReports.load(),d->receivedReports.load(),d->nacksSent.load(),d->retransmittedPackets.load()};}
bool MediaTransport::selectedRelay(bool bulk)const{
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 if(!d->pc[bulk])return false;rtc::Candidate local,remote;if(!d->pc[bulk]->getSelectedCandidatePair(&local,&remote))return false;return local.type()==rtc::Candidate::Type::Relayed&&remote.type()==rtc::Candidate::Type::Relayed;
#else
 return false;
#endif
}
bool MediaTransport::gatheringComplete(bool bulk)const{
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 return d->pc[bulk]&&d->gathered[bulk].load(std::memory_order_acquire);
#else
 Q_UNUSED(bulk);return false;
#endif
}
bool MediaTransport::readyForManualExport()const{return gatheringComplete(false)&&gatheringComplete(true);}
QString MediaTransport::aggregatedDescription(bool bulk,QString*type,QString*fingerprint)const{
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 // Only after GatheringState::Complete does the local description carry every
 // candidate. Returning it earlier would export an unusable half-offer.
 if(!gatheringComplete(bulk))return {};
 auto description=d->pc[bulk]->localDescription();if(!description)return {};
 if(type)*type=QString::fromStdString(description->typeString());
 if(fingerprint){auto value=description->fingerprint();*fingerprint=value?normalize(QString::fromStdString(value->value)):QString{};}
 return QString::fromStdString(std::string(*description));
#else
 Q_UNUSED(bulk);Q_UNUSED(type);Q_UNUSED(fingerprint);return {};
#endif
}
LinkState MediaTransport::linkState(bool bulk)const{
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 return d->pc[bulk]?LinkState(d->link[bulk].load(std::memory_order_acquire)):LinkState::Closed;
#else
 Q_UNUSED(bulk);return LinkState::Closed;
#endif
}
LinkState MediaTransport::aggregateLinkState()const{
 if(d->lite)return linkState(false);
 const auto control=linkState(false),bulk=linkState(true);
 const auto rank=[](LinkState s){switch(s){case LinkState::Failed:return 0;case LinkState::Closed:return 1;case LinkState::Disconnected:return 2;case LinkState::New:return 3;case LinkState::Connecting:return 4;case LinkState::Connected:return 5;}return 3;};
 return rank(control)<=rank(bulk)?control:bulk;
}
bool MediaTransport::requestRetransmission(quint64 frame){
#ifdef PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL
 std::lock_guard lock(d->mutex);if(!d->hasRx||!d->receivedSequence||frame<d->nextRx||frame>d->lastReceivedFrame||(d->lastReceivedFrame-frame)%960||!d->track||!d->track->isOpen()||monotonicNanos()-d->lastNack<20000000)return false;
 QByteArray nack(16,0);auto*p=reinterpret_cast<uchar*>(nack.data());p[0]=0x81;p[1]=205;qToBigEndian<quint16>(3,p+2);qToBigEndian<quint32>(d->rx.ssrc,p+8);qToBigEndian<quint16>(quint16(d->lastReceivedSequence-(d->lastReceivedFrame-frame)/960),p+12);try{d->track->send(reinterpret_cast<const std::byte*>(nack.constData()),nack.size());++d->nacksSent;d->lastNack=monotonicNanos();return true;}catch(...){return false;}
#else
 return false;
#endif
}
void MediaTransport::inheritProducerHistory(MediaTransport& previous){
 if(&previous==this)return;
 std::scoped_lock lock(d->mutex,previous.d->mutex);
 d->replaySource=previous.d->sourceHistory;
}
void MediaTransport::enableAutomaticManifest(bool enabled){std::lock_guard lock(d->mutex);d->automaticManifest=enabled;if(!enabled)d->waitingManifestAck=false;}
bool MediaTransport::acknowledgeSendManifest(const QString& id){std::lock_guard lock(d->mutex);if(!d->automaticManifest||!d->waitingManifestAck||!d->hasTx||id!=d->tx.streamId)return false;d->waitingManifestAck=false;return true;}
PcmRing& MediaTransport::decodedRing(){return d->decoded;}
PcmRing& MediaTransport::preCodecRing(){return d->preCodec;}
}
