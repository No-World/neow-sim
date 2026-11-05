CHARS = "0123456789ABCDEFGHIJKLMNPQRSTUVWXYZ"   # 33 个字符，无 O
def seed_to_str(seed):
    seed = int(seed)                              # .run 里可能存成字符串
    v = seed & ((1 << 64) - 1)                    # Long.toUnsignedString
    out=[]
    while v:
        out.append(CHARS[v % len(CHARS)])
        v //= len(CHARS)
    return "".join(reversed(out)) or "0"
def str_to_seed(s):
    s = s.upper().replace("O","0")
    total = 0
    for ch in s:
        total = total*len(CHARS) + CHARS.index(ch)
    # 回绕成有符号 64 位
    return total - (1<<64) if total >= (1<<63) else total
# 自检：往返一致
for s in ("PMCWKZEF","TRACE02A","ABCDE23"):
    assert seed_to_str(str_to_seed(s)) == s, (s, seed_to_str(str_to_seed(s)))
print("往返自检 OK")
if __name__=="__main__":
    import sys
    for x in sys.argv[1:]:
        print(x, "->", seed_to_str(int(x)))
