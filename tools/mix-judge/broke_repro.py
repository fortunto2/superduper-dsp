#!/usr/bin/env python3
"""IT BROKE — воспроизведение провала, а не рассказ о нём.

Я опубликовал конкурс по обработке звука, где судья оценивает без ушей,
и вместе с ним первую версию метрики. Она награждала починку, сделанную
В ПРОТИВОПОЛОЖНУЮ СТОРОНУ, баллом лучше, чем бездействие.

Запусти: uv run --with numpy --with scipy python broke_repro.py
Сигнал синтетический, файлы не нужны, результат детерминирован (seed=7).
"""
import numpy as np
from scipy.signal import stft

def _mono(x): return x.mean(axis=1) if x.ndim>1 else x

def lsd(a,b,sr,nffts=(2048,512,8192)):
    t=[]
    for n in nffts:
        A=np.abs(stft(_mono(a),fs=sr,nperseg=n,noverlap=n//2)[2])+1e-10
        B=np.abs(stft(_mono(b),fs=sr,nperseg=n,noverlap=n//2)[2])+1e-10
        m=min(A.shape[1],B.shape[1])
        t.append(np.sqrt(np.mean((20*np.log10(A[:,:m])-20*np.log10(B[:,:m]))**2)))
    return float(np.mean(t))

def bands(x,sr,e=(20,120,500,2000,8000,20000)):
    X=np.abs(np.fft.rfft(_mono(x))); f=np.fft.rfftfreq(len(_mono(x)),1/sr)
    return np.array([10*np.log10(np.sum(X[(f>=lo)&(f<hi)]**2)+1e-12) for lo,hi in zip(e[:-1],e[1:])])

def crest(x):
    m=_mono(x); return float(20*np.log10(np.max(np.abs(m))/(np.sqrt(np.mean(m**2))+1e-12)))

def BROKEN_score(ref,sub,sr):
    """КАК БЫЛО: одно число, веса взяты с потолка."""
    n=min(len(ref),len(sub)); ref,sub=ref[:n],sub[:n]
    g=np.sqrt(np.mean(_mono(ref)**2))/(np.sqrt(np.mean(_mono(sub)**2))+1e-12); sub=sub*g
    return lsd(ref,sub,sr) + 0.5*float(np.max(np.abs(bands(ref,sr)-bands(sub,sr)))) + 2*abs(crest(ref)-crest(sub))

def FIXED_axes(ref,sub,sr):
    """КАК СТАЛО: оси раздельно, потом нормировка на бездействие."""
    n=min(len(ref),len(sub)); ref,sub=ref[:n],sub[:n]
    g=np.sqrt(np.mean(_mono(ref)**2))/(np.sqrt(np.mean(_mono(sub)**2))+1e-12); sub=sub*g
    return {"спектр":lsd(ref,sub,sr),
            "баланс":float(np.max(np.abs(bands(ref,sr)-bands(sub,sr)))),
            "динамика":abs(crest(ref)-crest(sub))}

def tilt(y,sr,db):
    X=np.fft.rfft(y,axis=0); f=np.fft.rfftfreq(len(y),1/sr)
    sl=np.clip(np.log2((f+20)/1000),-4,4)
    return np.fft.irfft(X*(10**((sl*db/6)/20))[:,None],n=len(y),axis=0)

sr=44100; rng=np.random.default_rng(7); n=sr*20
t=np.arange(n)/sr
# «музыка»: розовый шум + тон + удары, лишь бы был широкий спектр и транзиенты
x=rng.normal(0,1,n); X=np.fft.rfft(x); f=np.fft.rfftfreq(n,1/sr)
x=np.fft.irfft(X/np.sqrt(np.maximum(f,20)),n=n)
x+=0.3*np.sin(2*np.pi*110*t)*(1+0.5*np.sin(2*np.pi*0.5*t))
env=np.zeros(n)
for k in range(0,n,sr//2): env[k:k+2000]+=np.exp(-np.linspace(0,8,min(2000,n-k)))
x+=1.2*env*rng.normal(0,1,n)
x/=np.abs(x).max()*1.05
M=np.stack([x, np.roll(x,13)*0.9+0.1*rng.normal(0,0.05,n)],axis=1)   # эталон, стерео

# Условия, при которых это и случилось: порча состоит из ДВУХ частей —
# слабого наклона и сильного пережатия, а участник чинит только наклон.
# Тогда в сумме со взятыми с потолка весами доминирует член динамики,
# одинаковый у обеих починок, и разница по спектру в нём тонет.
TILT=+1.5
AMT=0.75

def squash(y, amt):
    m=np.abs(y).max()+1e-12
    return np.sign(y)*(np.abs(y)/m)**(1-amt*0.5)*m

D=squash(tilt(M,sr,TILT), AMT)          # порча: наклон + пережатие
right=tilt(D,sr,-TILT)                  # чинит наклон в правильную сторону
wrong=tilt(D,sr,+TILT)                  # чинит наклон В ТУ ЖЕ СТОРОНУ, делая хуже

print(f"порча: наклон +{TILT} dB и пережатие {AMT}; участник чинит только наклон\n")
print("=== БЫЛО: одно число со взятыми с потолка весами")
b_none, b_right, b_wrong = BROKEN_score(M,D,sr), BROKEN_score(M,right,sr), BROKEN_score(M,wrong,sr)
print(f"  ничего не делать      {b_none:8.3f}")
print(f"  починил правильно     {b_right:8.3f}")
print(f"  починил В МИНУС       {b_wrong:8.3f}   <-- ЭТО ПРОВАЛ, если он <= 'ничего не делать'")
print(f"\n  провал воспроизведён: {b_wrong <= b_none}   (порча наизнанку оценена не хуже бездействия)")

print("\n=== СТАЛО: оси раздельно, нормировка на бездействие (1.0 = не тронул)")
base=FIXED_axes(M,D,sr); res={}
for name,cand in (("ничего не делать",D),("починил правильно",right),("починил В МИНУС",wrong)):
    a=FIXED_axes(M,cand,sr)
    rel={k:(a[k]/base[k] if base[k]>1e-9 else 0.0) for k in a}; res[name]=rel
    print(f"  {name:<20} худшая ось {max(rel.values()):6.3f}  " + " ".join(f"{k} {v:.3f}" for k,v in rel.items()))

r,w=res["починил правильно"],res["починил В МИНУС"]
print("\n=== И ЧЕГО ПОЧИНКА НЕ ЗАКРЫЛА (это тоже часть отчёта)")
print(f"  по спектру и балансу правильная лучше: {r['спектр']:.3f}/{r['баланс']:.3f} против {w['спектр']:.3f}/{w['баланс']:.3f}")
print(f"  но по динамике хуже: {r['динамика']:.3f} против {w['динамика']:.3f} — наклон в минус случайно")
print( "  приблизил крест-фактор к эталонному, и это перевешивает.")
print(f"  среднее:      правильная {np.mean(list(r.values())):.3f}  против  в минус {np.mean(list(w.values())):.3f}")
print(f"  худшая ось:   правильная {max(r.values()):.3f}  против  в минус {max(w.values()):.3f}")
print("\n  ВЫВОД: обе версии метрики путают их, когда участник НЕ ТРОНУЛ доминирующую")
print("  часть порчи (здесь пережатие). Разделение осей убрало произвольные веса,")
print("  но не сделало метрику способной ранжировать двух одинаково неполных участников.")
print("  Честная граница: судья годен отличать сделанное от несделанного, а не")
print("  сравнивать две половинчатые починки между собой.")
