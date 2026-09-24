import re
UNITS={w:i for i,w in enumerate("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen".split())}
UNITS["oh"]=0
TENS={w:10*i for i,w in enumerate("_ _ twenty thirty forty fifty sixty seventy eighty ninety".split()) if w!="_"}
SCALES={"hundred":100,"thousand":1000,"million":1000000}
ORD={"first":1,"second":2,"third":3,"fifth":5,"eighth":8,"ninth":9,"twelfth":12}
def tokens(t): return re.findall(r"[a-z]+|\d+", t.lower().replace("-", " "))
def spoken_numbers(text):
    """All values a transcript's number words and digits could denote."""
    out=set(); toks=tokens(text)
    for t in toks:
        if t.isdigit(): out.add(int(t))
    i=0
    while i<len(toks):
        if toks[i] in UNITS or toks[i] in TENS or toks[i] in SCALES or toks[i] in ORD or (toks[i]=="a" and i+1<len(toks) and toks[i+1] in SCALES):
            j=i; span=[]
            while j<len(toks) and (toks[j] in UNITS or toks[j] in TENS or toks[j] in SCALES or toks[j] in ORD or toks[j] in ("and","a")):
                span.append(toks[j]); j+=1
            words=[w for w in span if w not in ("and",)]
            # each word alone
            for w in words:
                for d in (UNITS,TENS,SCALES,ORD):
                    if w in d: out.add(d[w])
            # standard composition, e.g. "a hundred and fifty", "two hundred", "twenty two"
            total=cur=0
            for w in words:
                if w=="a": cur=max(cur,1); continue
                if w in UNITS: cur+=UNITS[w]
                elif w in TENS: cur+=TENS[w]
                elif w in ORD: cur+=ORD[w]
                elif w in SCALES:
                    cur=max(cur,1)*SCALES[w]
                    if SCALES[w]>=1000: total+=cur; cur=0
            out.add(total+cur)
            # adjacent pairs: "twenty two" -> 22, "two thirty" -> 2, 30
            vals=[TENS.get(w, UNITS.get(w, ORD.get(w))) for w in words if w not in SCALES and w!="a"]
            for a,b in zip(vals,vals[1:]):
                if a is not None and b is not None and a>=20 and a%10==0 and b<10: out.add(a+b)
            # digit strings: "four oh two" -> 402
            if len(vals)>=2 and all(v is not None and v<10 for v in vals):
                out.add(int("".join(str(v) for v in vals)))
            i=j
        else: i+=1
    return out
def invented_numbers(inp,out):
    allowed=spoken_numbers(inp)
    return [int(n) for n in re.findall(r"\d+", out) if int(n) not in allowed]
