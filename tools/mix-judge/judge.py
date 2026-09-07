"""Судья для конкурса по обработке звука. Оценка без ушей.

Схема: у судьи есть эталон M. Он применяет к нему известную ПОРЧУ D
(tilt-EQ + пережатие + сужение стерео) и раздаёт D(M). Участник возвращает
R(D(M)). Балл = расстояние от R(D(M)) до M. Ground truth однозначен,
вкус не участвует, подгонка под метрику невозможна: порча случайна и
тестовый набор скрыт.
"""
import numpy as np
from scipy.signal import stft, resample_poly

def _mono(x): return x.mean(axis=1) if x.ndim>1 else x

def log_spec_dist(a, b, sr, nffts=(2048, 512, 8192)):
    """Мультиразрешающее лог-спектральное расстояние, dB RMS."""
    tot=[]
    for n in nffts:
        A=np.abs(stft(_mono(a), fs=sr, nperseg=n, noverlap=n//2)[2])+1e-10
        B=np.abs(stft(_mono(b), fs=sr, nperseg=n, noverlap=n//2)[2])+1e-10
        m=min(A.shape[1],B.shape[1])
        d=20*np.log10(A[:,:m])-20*np.log10(B[:,:m])
        tot.append(np.sqrt(np.mean(d**2)))
    return float(np.mean(tot))

def band_tilt(a, b, sr, edges=(20,120,500,2000,8000,20000)):
    """Отклонение баланса по полосам, dB — ловит перекошенный спектр."""
    def bands(x):
        X=np.abs(np.fft.rfft(_mono(x))); f=np.fft.rfftfreq(len(_mono(x)),1/sr)
        return np.array([10*np.log10(np.sum(X[(f>=lo)&(f<hi)]**2)+1e-12)
                         for lo,hi in zip(edges[:-1],edges[1:])])
    return bands(a)-bands(b)

def stereo_width(x):
    if x.ndim<2 or x.shape[1]<2: return 0.0
    l,r=x[:,0],x[:,1]
    s=np.std(l-r); m=np.std(l+r)
    return float(s/(m+1e-12))

def crest_db(x):
    m=_mono(x); rms=np.sqrt(np.mean(m**2))+1e-12
    return float(20*np.log10(np.max(np.abs(m))/rms))

def content_dist(ref, sub, sr, win=0.25, lag_ms=50):
    """Пятая ось — временное содержание. Ловит «правильный спектр, чужая музыка».

    Первые четыре оси меряют глобальный профиль, и его можно подделать:
    карикатура (спектр эталона, случайные фазы) получала ИТОГ 1.072 при
    корреляции с эталоном 0.02 — то есть содержания нет вовсе, а судья
    отличал её от честного бездействия на 7%. Дыру назвал
    quiet-visitor-5302 на доске; здесь она закрыта.

    1 - медианный пик взаимной корреляции по окнам 250 мс с допуском
    сдвига ±50 мс. Допуск нужен, чтобы честная задержка обработки не
    читалась как потеря содержания.
    """
    a = _mono(ref); b = _mono(sub)
    n = min(len(a), len(b)); a, b = a[:n], b[:n]
    W = int(sr * win); L = int(sr * lag_ms / 1000); rs = []
    for st in range(0, n - W, W):
        x = a[st:st + W]
        y = b[max(0, st - L):min(n, st + W + L)]
        if x.std() < 1e-9 or y.std() < 1e-9:
            continue
        c = np.correlate(y - y.mean(), x - x.mean(), mode='valid')
        c /= (np.sqrt(np.sum((x - x.mean()) ** 2))
              * np.sqrt(np.sum((y[:W] - y[:W].mean()) ** 2)) + 1e-12)
        rs.append(float(np.max(np.abs(c))))
    return 1.0 - float(np.median(rs)) if rs else 1.0


def align(ref, sub, sr, max_lag_s=0.5):
    """Компенсировать задержку обработки перед оценкой.

    Stranger-check, предложенный postingboard: identity/pass-through обязан
    давать ровно 1.000. Он и давал — пока сигнал шёл в памяти. Стоило
    добавить честную задержку, и судья начал наказывать за неё:

        сдвиг      спектр   содержание
        64 смп     1.0065     0.9640
        1024       1.0927     1.0318
        4096       1.1689    42.8756

    Ось спектра разъезжается уже на 1.5 мс (кадры STFT), ось содержания
    рушится за пределом своего допуска ±50 мс. Участник, чья цепочка имеет
    латентность — а у любой FFT-обработки она есть, — штрафовался за то,
    что ничего не испортил.

    Лаг ищется по кросс-корреляции огибающих и возвращается вместе с
    результатом: компенсировать его правильно, а молча скрывать нельзя.
    """
    a=_mono(ref); b=_mono(sub)
    n=min(len(a),len(b)); a,b=a[:n],b[:n]
    L=int(sr*max_lag_s)
    N=1<<int(np.ceil(np.log2(2*n)))
    c=np.fft.irfft(np.fft.rfft(b-b.mean(),N)*np.conj(np.fft.rfft(a-a.mean(),N)),N)
    c=np.concatenate([c[-L:], c[:L+1]])
    lag=int(np.argmax(c))-L
    if lag>0:      sub2=sub[lag:]
    elif lag<0:    sub2=np.vstack([np.zeros((-lag,)+sub.shape[1:]), sub])
    else:          sub2=sub
    m=min(len(ref),len(sub2))
    return ref[:m], sub2[:m], lag


def _axes(ref, sub, sr):
    """Пять независимых осей в физических единицах."""
    n=min(len(ref),len(sub)); ref,sub=ref[:n],sub[:n]
    g=np.sqrt(np.mean(_mono(ref)**2))/(np.sqrt(np.mean(_mono(sub)**2))+1e-12)
    sub=sub*g                       # громкость нормируется: конкурс не про «сделай громче»
    return {
        "спектр":  log_spec_dist(ref,sub,sr),
        "баланс":  float(np.max(np.abs(band_tilt(ref,sub,sr)))),
        "динамика":abs(crest_db(ref)-crest_db(sub)),
        "стерео":  abs(stereo_width(ref)-stereo_width(sub)),
        "содержание": content_dist(ref,sub,sr),
    }

def score(ref, sub, damaged, sr):
    """Балл по каждой оси В ЕДИНИЦАХ БЕЗДЕЙСТВИЯ: 1.0 = не тронул,
    0 = попал в эталон, >1 = сделал хуже, чем было.

    Возвращает пять осей, `худшая_ось` (gate), `ИТОГ` (среднее по прошедшим),
    `задержка_смп` (снятый лаг обработки) и `сырое` — оси в физических единицах.
    """
    ref_a, sub_a, lag = align(ref, sub, sr)
    a=_axes(ref_a,sub_a,sr); base=_axes(ref,damaged,sr)
    rel={k: (a[k]/base[k] if base[k]>1e-9 else 0.0) for k in a}
    axes=("спектр","баланс","динамика","стерео","содержание")
    # Два числа, а не одно. Просто среднее — это тайные равные веса
    # (замечание postingboard): оно размывает одну провальную ось
    # четырьмя приличными, и карикатура с осью содержания 50.6 утонула бы
    # в нём. Просто максимум — другая крайность: он завалил honest-случай,
    # который улучшил три оси и на 22% ухудшил четвёртую.
    #
    # Поэтому gate + балл. GATE = худшая ось: сделал что-то В ПОЛТОРА РАЗА
    # хуже, чем было — не участвуешь, сколько бы ни выиграл в другом.
    # БАЛЛ (среднее) ранжирует только прошедших.
    worst=float(np.max([rel[k] for k in axes]))
    rel["худшая_ось"]=worst
    rel["прошёл_gate"]=bool(worst < 1.5)
    rel["ИТОГ"]=float(np.mean([rel[k] for k in axes]))
    rel["задержка_смп"]=lag
    return {**{k:(round(v,4) if isinstance(v,float) else v) for k,v in rel.items()},
            "сырое":{k:round(v,3) for k,v in a.items()}}

def damage(x, sr, seed=0):
    """Порча: наклон спектра + пережатие + сужение стерео. Параметры от seed."""
    rng=np.random.default_rng(seed)
    y=x.copy().astype(np.float64)
    # 1. наклон спектра (tilt) случайной крутизны
    tilt_db=rng.uniform(-6,6)
    X=np.fft.rfft(y,axis=0); f=np.fft.rfftfreq(len(y),1/sr)
    sl=np.clip(np.log2((f+20)/1000),-4,4)
    X*= (10**((sl*tilt_db/6)/20))[:,None] if y.ndim>1 else 10**((sl*tilt_db/6)/20)
    y=np.fft.irfft(X,n=len(y),axis=0)
    # 2. пережатие (мгновенная компрессия по огибающей)
    amt=rng.uniform(0.3,0.8)
    m=np.abs(y).max()+1e-12
    y=np.sign(y)*(np.abs(y)/m)**(1-amt*0.5)*m
    # 3. сужение стерео
    if y.ndim>1 and y.shape[1]>1:
        w=rng.uniform(0.2,0.8)
        mid=(y[:,0]+y[:,1])/2; side=(y[:,0]-y[:,1])/2*w
        y=np.stack([mid+side,mid-side],axis=1)
    return y/ (np.abs(y).max()+1e-12) * (np.abs(x).max()+1e-12), {"tilt_dB":round(tilt_db,2),"пережатие":round(amt,2)}
