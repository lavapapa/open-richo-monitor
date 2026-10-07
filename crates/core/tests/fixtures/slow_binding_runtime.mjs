import readline from 'node:readline';
const lines=readline.createInterface({input:process.stdin});
const reply=(id,result)=>process.stdout.write(JSON.stringify({id,result})+'\n');
lines.on('line',async line=>{
 const {id,method,params}=JSON.parse(line);
 if(method==='begin_binding'){
  await new Promise(r=>setTimeout(r,350));
  const binding={id:params.bindingId,provider:params.provider,status:'waiting',targets:[],qrUrl:'https://example.invalid/qr'};
  process.stdout.write(JSON.stringify({event:'binding',data:binding})+'\n');
  reply(id,binding);
 } else reply(id,{});
});
lines.on('close',()=>process.exit(0));
