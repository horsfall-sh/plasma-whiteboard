// The JavaScript side of tests/parity.rs: lifts geom and view functions out of app.js (argv[2])
// and runs the cases on stdin through them.
const fs=require('fs'); const src=fs.readFileSync(process.argv[2],'utf8');
const grab=(name)=>{const i=src.indexOf('function '+name+'('); let d=0,j=src.indexOf('{',i); for(;;j++){if(src[j]=='{')d++; else if(src[j]=='}'){d--; if(!d)break;}} return src.slice(i,j+1);};
const ASPECT=9/16, WX=-1, WY=-ASPECT, WW=3, WH=3*ASPECT, OVERLAP=1.08, MAX_ZOOM=8, CORNER_COS=0.35;
const r5=(v)=>Math.round(v*1e5)/1e5; const clamp=(v,lo,hi)=>Math.min(hi,Math.max(lo,v));
const clampPan=(p,vis,lo,size)=>(vis>=size?lo+(size-vis)/2:Math.min(lo+size-vis,Math.max(lo,p)));
let vw,vh,bw,zoom,panX,panY,laidOut,dpr; const visW=(z)=>vw/(bw*z), visH=(z)=>vh/(bw*z), minZoom=()=>Math.min(vw/WW,vh/WH)/bw;
let rec; const ctx={beginPath(){rec.push(['b'])},moveTo(x,y){rec.push(['m',x,y])},quadraticCurveTo(a,b,c,d){rec.push(['q',a,b,c,d])},
 bezierCurveTo(a,b,c,d,e,f){rec.push(['c',a,b,c,d,e,f])},stroke(){rec.push(['s'])},arc(x,y,r){rec.push(['d',x,y,r])},fill(){},
 set lineWidth(w){rec.push(['w',w])}};
const noop=()=>{}; const placePaper=noop, stretchBase=noop, redrawLive=noop, placeEditor=noop, showZoom=noop, redrawBase=noop, settleView=noop, offScreen=()=>false, drawText=noop;
const prep=noop; let innerWidth, innerHeight, devicePixelRatio=1; const base={}, live={}, spare={};
eval([ 'curveThrough','steady','lineW','dot','drawFull','hueHex','setView','zoomAt','zoomHome','pageAt','goPage','pageStep','layout'].map(grab).join('\n')
  .replace('clearTimeout(viewTimer);','').replace(/if \(settle\) settleView\(\); else viewTimer = setTimeout\(settleView, 140\);/,''));
const cases=JSON.parse(fs.readFileSync(0,'utf8')); const out=[];
for(const c of cases){ bw=c.bw; const ct=curveThrough(c.pts,c.px), st=steady(c.pts,c.px); rec=[]; drawFull(ctx,{pts:c.pts,w:c.w,c:'#000'}); const full=rec;
 laidOut=false; zoom=1;panX=0;panY=0; innerWidth=c.vw; innerHeight=c.vh; layout(); const views=[];
 for(const a of c.acts){ if(a[0]=='za')zoomAt(a[1],a[2],a[3],true); else if(a[0]=='home')zoomHome(); else if(a[0]=='ps')pageStep(a[1],a[2]); else if(a[0]=='set')setView(a[1],a[2],a[3],true); else if(a[0]=='rs'){innerWidth=a[1];innerHeight=a[2];layout();}
  const W=visW(zoom),H=visH(zoom),all=W>=WW-1e-6&&H>=WH-1e-6,e=1e-4;
  views.push([zoom,panX,panY,all||panX<=WX+e,all||panX+W>=WX+WW-e,all||panY<=WY+e,all||panY+H>=WY+WH-e]); }
 out.push({ct,st,full,hue:hueHex(c.hue),views}); }
console.log(JSON.stringify(out));
