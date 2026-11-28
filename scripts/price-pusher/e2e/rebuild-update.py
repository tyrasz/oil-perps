# Rebuild a Hermes accumulator update (PNAU: VAA + one Merkle price update) for
# XAU/USD from a mainnet push-oracle transaction fetched with getTransaction
# (encoding json, maxSupportedTransactionVersion 1) and saved as a one-item list.
#   python3 rebuild-update.py tx.json   (writes hermes_xau.json, clone.json)
import json,struct,base64
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(s):
    n=0
    for c in s: n=n*58+A.index(c)
    b=n.to_bytes((n.bit_length()+7)//8,'big') if n else b''
    return b'\0'*(len(s)-len(s.lstrip('1')))+b
import sys
t=json.load(open(sys.argv[1]))[0]
m=t['transaction']['message']; keys=m['accountKeys']+t['meta'].get('loadedAddresses',{}).get('writable',[])+t['meta'].get('loadedAddresses',{}).get('readonly',[])
ixs=m['instructions']
write=[b58(i['data']) for i in ixs if keys[i['programIdIndex']].startswith('HDw2') and len(b58(i['data']))>50]
assert len(write)==1
w=write[0]; idx,ln=struct.unpack_from('<II',w,8); vaa=w[16:16+ln]; assert idx==0 and len(vaa)==ln
verify=[i for i in ixs if keys[i['programIdIndex']].startswith('HDw2') and len(i['accounts'])==3][0]
upd=[i for i in ixs if keys[i['programIdIndex']].startswith('pyt2') and keys[i['accounts'][5]].startswith('csXV1a')][0]
d=b58(upd['data']); o=8
mlen=struct.unpack_from('<I',d,o)[0]; o+=4; msg=d[o:o+mlen]; o+=mlen
np_=struct.unpack_from('<I',d,o)[0]; o+=4; proof=d[o:o+20*np_]; o+=20*np_
treasury_id=d[o]; o+=1; shard=struct.unpack_from('<H',d,o)[0]; o+=2; fid=d[o:o+32]
print('vaa',len(vaa),'msg',len(msg),'proofs',np_,'treasury',treasury_id,'shard',shard,'feed',fid.hex()[:12])
acc=b'PNAU'+bytes([1,0,0,0])+struct.pack('>H',len(vaa))+vaa+bytes([1])+struct.pack('>H',len(msg))+msg+bytes([np_])+proof
mt,=struct.unpack_from('>B',msg,0); f=msg[1:33]; price,conf,expo,pt,ppt,ema,emac=struct.unpack_from('>qQiqqqQ',msg,33)
print('type',mt,'price',price,'conf',conf,'expo',expo,'publish',pt,'ema',ema)
json.dump({'binary':{'encoding':'base64','data':[base64.b64encode(acc).decode()]},'parsed':[{'id':f.hex(),'price':{'price':str(price),'conf':str(conf),'expo':expo,'publish_time':pt},'ema_price':{'price':str(ema),'conf':str(emac),'expo':expo,'publish_time':pt},'metadata':{}}]},open('hermes_xau.json','w'))
clone={'guardian_set':keys[verify['accounts'][2]],'receiver_config':keys[upd['accounts'][3]],'treasury':keys[upd['accounts'][4]],'signature':t['transaction']['signatures'][0],'slot':t['slot']}
json.dump(clone,open('clone.json','w'),indent=1); print(clone)
