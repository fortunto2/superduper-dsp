"""Открытый вопрос конкурса №2: чем усложнить порчу, чтобы задача не была
«угадай три числа». Ответ проверяю, а не предлагаю."""
import sys, numpy as np, soundfile as sf
sys.path.insert(0,sys.argv[1])
from judge import score, damage

def out_of_family(x, sr, seed=0):
    """Порча ВНЕ семейства: короткая ранняя реверберация + мягкий клип.
    Ни одна из двух не выражается через (наклон, пережатие, ширина),
    то есть обращением известного семейства не снимается."""
    rng=np.random.default_rng(seed+100)
    y=x.copy()
    # 1. ранние отражения: 6 задержек 7..47 мс с затуханием, чуть разные в каналах
    out=y.copy()
    for k in range(6):
        d=int(sr*rng.uniform(0.007,0.047)); g=rng.uniform(0.12,0.30)*(0.85**k)
        for ch in range(y.shape[1]):
            dd=d+(3 if ch else 0)
            out[dd:,ch]+=g*y[:-dd,ch]
    # 2. мягкий клип (нелинейность, а не степенная компрессия из семейства)
    m=np.abs(out).max()+1e-12
    out=np.tanh(out/m*rng.uniform(1.2,2.2))*m
    return out/(np.abs(out).max()+1e-12)*(np.abs(x).max()+1e-12)

def cheat(y, sr, par, w=0.5):
    """Читер: обращает ТОЛЬКО известное семейство."""
    t,a=par['tilt_dB'],par['пережатие']
    X=np.fft.rfft(y,axis=0); f=np.fft.rfftfreq(len(y),1/sr)
    sl=np.clip(np.log2((f+20)/1000),-4,4)
    z=np.fft.irfft(X*(10**((-sl*t/6)/20))[:,None],n=len(y),axis=0)
    m=np.abs(z).max()+1e-12
    z=np.sign(z)*(np.abs(z)/m)**(1/(1-a*0.5))*m
    mid=(z[:,0]+z[:,1])/2; side=(z[:,0]-z[:,1])/2/max(w,0.05)
    return np.stack([mid+side,mid-side],axis=1)

sig,sr=sf.read(sys.argv[2],always_2d=True); sig=sig[:sr*30].astype(np.float64)

print(f"{'схема порчи':<34}{'бездействие':>12}{'читер':>10}   вывод")
# v1: только семейство
d1,par=damage(sig,sr,seed=7)
c1=cheat(d1,sr,par)
n=min(len(c1),len(d1)); s1=score(sig[:n],c1[:n],d1[:n],sr)['ИТОГ']
print(f"{'v1: только семейство D':<34}{1.000:>12.3f}{s1:>10.3f}   читер выигрывает втрое")

# v2: семейство + порча вне семейства
d2=out_of_family(d1,sr,seed=7)
c2=cheat(d2,sr,par)
n=min(len(c2),len(d2)); s2=score(sig[:n],c2[:n],d2[:n],sr)['ИТОГ']
det=score(sig[:n],d2[:n],d2[:n],sr)['ИТОГ']
print(f"{'v2: семейство + реверб/клип':<34}{det:>12.3f}{s2:>10.3f}   ", end="")
print("читер больше не выигрывает" if s2>0.85 else "читер всё ещё выигрывает")

s2f=score(sig[:n],c2[:n],d2[:n],sr)
print(f"\nчитер на v2 по осям: " + " ".join(f"{k} {s2f[k]:.3f}" for k in ("спектр","баланс","динамика","стерео","содержание")))
print(f"gate: {'прошёл' if s2f['прошёл_gate'] else 'ОТСЕЧЁН'}   худшая ось {s2f['худшая_ось']:.3f}")

# известный ответ: идеальный участник на v2 всё ещё достижим?
print(f"\nконтроль — эталон против v2: {score(sig[:n],sig[:n],d2[:n],sr)['ИТОГ']:.3f}  (обязан быть ~0: задача решаема)")

print("\n=== контроль: а не отсёкся ли читер по случайной причине (раздул стерео)?")
for w,label in ((0.5,"агрессивный (расширяет стерео)"),(1.0,"аккуратный (стерео не трогает)")):
    for tag,dd in (("v1",d1),("v2",d2)):
        c=cheat(dd,sr,par,w); n=min(len(c),len(dd))
        s=score(sig[:n],c[:n],dd[:n],sr)
        print(f"  {label:<32} {tag}: балл {s['ИТОГ']:6.3f}  худшая {s['худшая_ось']:6.3f}  "
              f"{'прошёл' if s['прошёл_gate'] else 'ОТСЕЧЁН'}   "
              f"спектр {s['спектр']:.3f} стерео {s['стерео']:.3f} содерж {s['содержание']:.3f}")
