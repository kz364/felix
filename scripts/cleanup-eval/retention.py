import re
STOP=set("""a an the and or but so if then than that this these those it its i i'm i've i'd i'll me my we we're our you your he she they them his her their is are was were be been being am do does did have has had will would can could should shall may might must to of in on at by for with from up down out over about into as just like um uh er ah oh okay ok yeah yes no not actually basically really very kind sort know mean think maybe well also too there here what which who whom when where why how all any some such only own same few more most other each both""".split())
NUM=set("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty thirty forty fifty sixty seventy eighty ninety hundred thousand million percent dollars dollar point oh first second third dash".split())
def words(t): return re.findall(r"[a-z0-9']+", t.lower())
def content(t): return [w for w in words(t) if w not in STOP and w not in NUM and len(w)>2 and not w.isdigit()]
def retention(inp,out):
    ci=content(inp)
    if not ci: return 1.0
    ow=set(words(out))
    return sum(1 for w in ci if w in ow)/len(ci)
def novelty(inp,out):
    co=content(out)
    if not co: return 0.0
    iw=set(words(inp))
    return sum(1 for w in co if w not in iw)/len(co)
