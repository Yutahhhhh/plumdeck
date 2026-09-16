#include "harness.h"
#include "junction/media_transport.h"
#include "junction/ice_servers.h"
#include <QTemporaryDir>
#include <thread>
#include <atomic>
#include <cmath>
#include <mutex>
#include <deque>
#include <QFile>
#include <QDateTime>
#include <QMessageAuthenticationCode>
#include <QUrl>
#include <rtc/rtc.hpp>
using namespace junction;
static void exerciseConnection(bool senderOffers,bool automatic=false,bool relay=false) {
 if(!MediaTransport::available())return;
 if(relay)rtc::InitLogger(rtc::LogLevel::Info,[](rtc::LogLevel,std::string message){if(message.find("ICE")!=std::string::npos||message.find("TURN server")!=std::string::npos||message.find("state changed")!=std::string::npos)std::fprintf(stderr,"%s\n",message.c_str());});
 QTemporaryDir directory;QString error;auto ai=MediaTransport::createIdentity(directory.path(),&error),bi=MediaTransport::createIdentity(directory.path(),&error);CHECK(ai&&bi);
 MediaTransport *a=nullptr,*b=nullptr;std::atomic<int> control{0},bulk{0},validation{0},errors{0};
 MediaTransport::Callbacks ac,bc;std::mutex manifestMutex;std::optional<StreamManifest> pendingManifest;int manifests=0;
 ac.producerManifest=[&](StreamManifest m){std::lock_guard lock(manifestMutex);pendingManifest=m;};
 std::mutex signalsMutex;std::deque<std::function<void()>> pendingSignals;
 auto enqueue=[&](std::function<void()> work){std::lock_guard lock(signalsMutex);pendingSignals.push_back(std::move(work));};
 auto pump=[&]{std::deque<std::function<void()>> batch;{std::lock_guard lock(signalsMutex);batch.swap(pendingSignals);}for(auto& work:batch)work();};
 ac.localDescription=[&](bool bulk,QString s,QString t,QString fp){enqueue([&,bulk,s,t,fp]{if(fp!=ai->fingerprint||b->remoteDescription(bulk,s,t,QString(64,'0'))||!b->remoteDescription(bulk,s,t,ai->fingerprint))++errors;});};
 bc.localDescription=[&](bool bulk,QString s,QString t,QString fp){enqueue([&,bulk,s,t,fp]{if(fp!=bi->fingerprint||a->remoteDescription(bulk,s,t,QString(64,'0'))||!a->remoteDescription(bulk,s,t,bi->fingerprint))++errors;});};
 ac.localCandidate=[&](bool bulk,QString c,QString mid){enqueue([&,bulk,c,mid]{if(relay)std::fprintf(stderr,"sender candidate relay=%d\n",int(c.contains(" typ relay ")));b->remoteCandidate(bulk,c,mid);});};
 bc.localCandidate=[&](bool bulk,QString c,QString mid){enqueue([&,bulk,c,mid]{if(relay)std::fprintf(stderr,"receiver candidate relay=%d\n",int(c.contains(" typ relay ")));a->remoteCandidate(bulk,c,mid);});};
 bc.validation=[&](QByteArray b){if(b==QByteArray(32840,'v'))++validation;};bc.control=[&](QByteArray b){if(b=="hello")++control;};bc.bulk=[&](QByteArray b){if(b=="asset")++bulk;};
 QStringList ice;
 if(relay){QFile secret(qEnvironmentVariable("JUNCTION_TURN_SECRET_FILE"));CHECK(secret.open(QIODevice::ReadOnly));const auto username=QByteArray::number(QDateTime::currentSecsSinceEpoch()+600)+":junction-test";const auto password=QMessageAuthenticationCode::hash(username,secret.readAll().trimmed(),QCryptographicHash::Sha1).toBase64();ice=iceServerUrls(QJsonArray{QJsonObject{{"urls",(qEnvironmentVariableIsSet("JUNCTION_TURN_TLS")?QString("turns:"):QString("turn:"))+qEnvironmentVariable("JUNCTION_TURN_ADDRESS")},{"username",QString::fromUtf8(username)},{"credential",QString::fromLatin1(password)}}});}
 MediaTransport sender("b",ice,ac,ai,relay),receiver("a",ice,bc,bi,relay);a=&sender;b=&receiver;struct Cleanup{MediaTransport& a;MediaTransport& b;~Cleanup(){a.close();b.close();}} cleanup{sender,receiver};sender.enableAutomaticManifest(automatic);
 StreamManifest m{"stream","a",1,1,48000,123456,9000};CHECK(receiver.setReceiveManifest(m));if(senderOffers){CHECK(sender.setSendManifest(m));CHECK(receiver.start(false,&error));CHECK(sender.start(true,&error));}else{CHECK(sender.start(false,&error));CHECK(receiver.start(true,&error));CHECK(sender.setSendManifest(m));}
 for(int i=0;i<(relay?800:200)&&(!control||!bulk||!validation);i++){pump();sender.sendControl("hello");sender.sendBulk("asset");sender.sendValidation(QByteArray(32840,'v'));std::this_thread::sleep_for(std::chrono::milliseconds(25));}CHECK(control>0);CHECK(bulk>0);CHECK(validation>0);CHECK_EQ(errors.load(),0);if(relay){CHECK(sender.selectedRelay());CHECK(receiver.selectedRelay());CHECK(sender.selectedRelay(true));CHECK(receiver.selectedRelay(true));}
 CHECK(sender.sendKeepAlive());const auto applicationBulk=bulk.load();std::this_thread::sleep_for(std::chrono::milliseconds(25));CHECK_EQ(bulk.load(),applicationBulk);
 PcmRing input(128,960,2);sender.startProducer(&input);float pcm[1920];bool manualNack=false;for(int j=0;j<65;j++){for(int i=0;i<960;i++){pcm[i*2]=.3f*std::sin((j*960+i)*.04f);pcm[i*2+1]=.2f*std::cos((j*960+i)*.03f);}PcmBlockInfo info;info.epoch=automatic&&j>=40?2:1;info.generation=automatic&&j>=40?2:1;info.mediaFrame=(automatic?48777:48000)+j*960;info.sourceFrame=j*960;info.sequence=j;info.frameCount=960;info.sampleRateHz=48000;info.channels=2;CHECK(input.push(pcm,info));if(j==55)for(int attempt=0;attempt<10&&!manualNack;++attempt){manualNack=receiver.requestRetransmission((automatic?48777:48000)+55*960);if(!manualNack)std::this_thread::sleep_for(std::chrono::milliseconds(1));}std::this_thread::sleep_for(std::chrono::milliseconds(20));if(automatic&&(j==3||j==43)){std::lock_guard lock(manifestMutex);CHECK(pendingManifest);if(j==3)CHECK_EQ(receiver.statistics().receivedPackets,0ULL);CHECK_EQ(pendingManifest->mediaFrameOrigin,quint64(j==3?48777:48777+40*960));CHECK(!sender.acknowledgeSendManifest("wrong-stream"));CHECK(receiver.setReceiveManifest(*pendingManifest));CHECK(sender.acknowledgeSendManifest(pendingManifest->streamId));++manifests;pendingManifest.reset();}}
 for(int i=0;i<100&&!receiver.decodedRing().availableFrames();i++)std::this_thread::sleep_for(std::chrono::milliseconds(10));auto r=receiver.decodedRing().popBlock(pcm,960);CHECK(r.frames>0);CHECK_EQ(r.info.epoch,1ULL);CHECK_EQ(r.info.mediaFrame,automatic?48777ULL:48000ULL);double energy=0;for(unsigned i=0;i<r.frames*2;i++)energy+=pcm[i]*pcm[i];CHECK(energy>1);double ll=0,rr=0,lr=0;for(unsigned i=0;i<r.frames;i++){ll+=pcm[i*2]*pcm[i*2];rr+=pcm[i*2+1]*pcm[i*2+1];lr+=pcm[i*2]*pcm[i*2+1];}CHECK(ll>1&&rr>1);CHECK(std::fabs(lr/std::sqrt(ll*rr))<.8);CHECK(sender.statistics().senderReports>0);CHECK(receiver.statistics().receivedReports>0);if(manualNack){CHECK(receiver.statistics().nacksSent>0);CHECK(sender.statistics().retransmittedPackets>0);}CHECK(!receiver.requestRetransmission(48000));if(automatic){CHECK_EQ(manifests,2);bool found=false;for(;;){auto got=receiver.decodedRing().popBlock(pcm,960);if(!got.frames)break;if(got.info.epoch==2)found=true;}CHECK(found);}sender.close();receiver.close();
}

JTEST("media-transport","real ICE DTLS stereo Opus isolated bulk RTCP and deadline repair") {exerciseConnection(true);}
JTEST("media-transport","guest sends after answer negotiation with authenticated stream manifest") {exerciseConnection(false);}

JTEST("media-transport","automatic manifest ACK anchors arbitrary frames and changes epoch safely") {exerciseConnection(true,true);}

JTEST("media-transport","Lite sendrecv carries independent stereo audio in both directions") {
 if(!MediaTransport::available())return;
 QTemporaryDir directory;QString error;auto hi=MediaTransport::createIdentity(directory.path(),&error),gi=MediaTransport::createIdentity(directory.path(),&error);CHECK(hi&&gi);
 MediaTransport host("guest",{}, {},hi),guest("host",{}, {},gi);struct Cleanup{MediaTransport& a;MediaTransport& b;~Cleanup(){a.close();b.close();}} cleanup{host,guest};
 StreamManifest hm{"host-return","host",7,1,0,101,7001},gm{"guest-program","guest",7,1,0,202,7002};
 CHECK(host.setSendManifest(hm));CHECK(guest.setSendManifest(gm));host.setBrowserReceive(7,1000);guest.setBrowserReceive(7,2000);
 CHECK(guest.startLite(false,&error));CHECK(host.startLite(true,&error));
 for(int i=0;i<100&&!host.gatheringComplete(false);++i)std::this_thread::sleep_for(std::chrono::milliseconds(10));CHECK(host.gatheringComplete(false));
 QString type,fp;const auto offer=host.aggregatedDescription(false,&type,&fp);CHECK(type=="offer");CHECK(offer.contains("a=sendrecv"));CHECK(guest.remoteDescription(false,offer,type,fp,&error));
 for(int i=0;i<100&&!guest.gatheringComplete(false);++i)std::this_thread::sleep_for(std::chrono::milliseconds(10));CHECK(guest.gatheringComplete(false));
 const auto answer=guest.aggregatedDescription(false,&type,&fp);CHECK(type=="answer");CHECK(host.remoteDescription(false,answer,type,fp,&error));
 for(int i=0;i<200&&(host.linkState(false)!=LinkState::Connected||guest.linkState(false)!=LinkState::Connected);++i)std::this_thread::sleep_for(std::chrono::milliseconds(10));CHECK(host.linkState(false)==LinkState::Connected);CHECK(guest.linkState(false)==LinkState::Connected);
 PcmRing hostPcm(64,960,2),guestPcm(64,960,2);host.startProducer(&hostPcm);guest.startProducer(&guestPcm);float a[1920],b[1920];
 for(int block=0;block<30;++block){for(int i=0;i<960;++i){a[i*2]=.2f*std::sin((block*960+i)*.031f);a[i*2+1]=.1f*std::cos((block*960+i)*.037f);b[i*2]=.15f*std::sin((block*960+i)*.047f);b[i*2+1]=.25f*std::cos((block*960+i)*.053f);}PcmBlockInfo info;info.epoch=7;info.generation=1;info.mediaFrame=block*960;info.sourceFrame=block*960;info.sequence=block;info.frameCount=960;info.sampleRateHz=48000;info.channels=2;CHECK(hostPcm.push(a,info));CHECK(guestPcm.push(b,info));std::this_thread::sleep_for(std::chrono::milliseconds(20));}
 for(int i=0;i<100&&(!host.decodedRing().availableFrames()||!guest.decodedRing().availableFrames());++i)std::this_thread::sleep_for(std::chrono::milliseconds(10));
 auto atHost=host.decodedRing().popBlock(a,960),atGuest=guest.decodedRing().popBlock(b,960);CHECK(atHost.frames>0);CHECK(atGuest.frames>0);double hostEnergy=0,guestEnergy=0;for(unsigned i=0;i<atHost.frames*2;++i)hostEnergy+=a[i]*a[i];for(unsigned i=0;i<atGuest.frames*2;++i)guestEnergy+=b[i]*b[i];CHECK(hostEnergy>1);CHECK(guestEnergy>1);
}

JTEST("media-relay","forced TURN with expiring REST credentials carries real media and isolated channels"){if(!qEnvironmentVariableIsSet("JUNCTION_TURN_ADDRESS"))jtest::skip("JUNCTION_TURN_ADDRESS is not configured");exerciseConnection(true,true,true);}

JTEST("ice-servers","TURN TLS stays encrypted and ephemeral credentials are escaped"){
 const auto urls=iceServerUrls(QJsonArray{QJsonObject{{"urls",QJsonArray{"turn:relay.example:3478?transport=udp","turns:relay.example:5349?transport=tcp"}},{"username","123:peer"},{"credential","a+b/c="}}});
 CHECK(urls.contains("stun:relay.example:3478"));
 CHECK(urls.contains("turn:123%3Apeer:a%2Bb%2Fc%3D@relay.example:3478?transport=udp"));
 CHECK(urls.contains("turns:123%3Apeer:a%2Bb%2Fc%3D@relay.example:5349?transport=tls"));
 CHECK(iceServerUrls(QJsonArray{QJsonObject{{"urls",QJsonArray{"https://invalid.example","turn:user@invalid.example"}}}}).isEmpty());
}
