"""Известные ответы судьи. Зелёный тест, который не видели красным, ничего не значит,
поэтому здесь каждый случай с заранее объявленным ожиданием."""
import sys, numpy as np, soundfile as sf
sys.path.insert(0,sys.argv[1] if len(sys.argv)>1 else '.')
from judge import score, damage
from scipy.signal import stft, istft

sig,sr=sf.read(sys.argv[2], always_2d=True); sig=sig[:sr*30].astype(np.float64)
dmg,par=damage(sig,sr,seed=7); print("порча:",par)

def undo_tilt(y,sr,t):
    X=np.fft.rfft(y,axis=0); f=np.fft.rfftfreq(len(y),1/sr)
    sl=np.clip(np.log2((f+20)/1000),-4,4)
    return np.fft.irfft(X*(10**((-sl*t/6)/20))[:,None],n=len(y),axis=0)

def d_inverse(y,sr,par,w=0.5):
    """Читер: знает СЕМЕЙСТВО порчи и обращает его, эталона не видя."""
    t,a=par['tilt_dB'],par['пережатие']
    z=undo_tilt(y,sr,t); m=np.abs(z).max()+1e-12
    z=np.sign(z)*(np.abs(z)/m)**(1/(1-a*0.5))*m
    mid=(z[:,0]+z[:,1])/2; side=(z[:,0]-z[:,1])/2/max(w,0.05)
    return np.stack([mid+side,mid-side],axis=1)

def caricature(y,sr):
    """Карикатура: спектр эталона сохранён, содержание уничтожено (случайные фазы
    в mid/side, чтобы не выдать себя осью стерео)."""
    rng=np.random.default_rng(2)
    mid=(y[:,0]+y[:,1])/2; side=(y[:,0]-y[:,1])/2; out=[]
    for comp in (mid,side):
        _,_,Z=stft(comp,fs=sr,nperseg=2048,noverlap=1536)
        Z=np.abs(Z)*np.exp(1j*rng.uniform(0,2*np.pi,Z.shape))
        _,x=istft(Z,fs=sr,nperseg=2048,noverlap=1536); out.append(x[:len(y)])
    n=min(len(o) for o in out); m,s_=out[0][:n],out[1][:n]
    return np.stack([m+s_,m-s_],axis=1)

# identity с честной задержкой обработки: у любой FFT-цепочки она есть,
# и штрафовать за неё нельзя. Stranger-check предложил postingboard.
def lagged(y, n):
    return np.vstack([np.zeros((n, y.shape[1])), y[:-n]])

cases=[
 ("эталон сам с собой",   sig,                        "gate ok, балл ~0"),
 ("ничего не делать",     dmg,                        "gate ok, балл 1.000"),
 ("починил наклон",       undo_tilt(dmg,sr,par['tilt_dB']), "gate ok, балл < 1"),
 ("читер: обратил D",     d_inverse(dmg,sr,par),      "gate ok, балл << 1 — ДЫРА"),
 ("починил В МИНУС",      undo_tilt(dmg,sr,-par['tilt_dB']), "балл > 1"),
 ("тише в 3 раза",        dmg*0.3,                    "ровно как бездействие"),
 ("карикатура",           caricature(dmg,sr),         "GATE ОБЯЗАН ОТСЕЧЬ"),
 ("шум вместо музыки",    np.random.default_rng(0).normal(0,0.1,dmg.shape), "GATE ОБЯЗАН ОТСЕЧЬ"),
 ("identity + 23 мс задержки", lagged(dmg,1024), "обязан остаться 1.000"),
 ("identity + 93 мс задержки", lagged(dmg,4096), "обязан остаться 1.000"),
]
print(f"\n{'вариант':<24}{'балл':>7}{'худшая':>8}{'gate':>7}  {'содерж':>7}{'спектр':>7}{'динам':>7}   ожидание")
res={}
for name,x,exp in cases:
    n=min(len(x),len(sig)); s=score(sig[:n],x[:n],dmg[:n],sr); res[name]=s
    print(f"{name:<24}{s['ИТОГ']:7.3f}{s['худшая_ось']:8.3f}{('OK' if s['прошёл_gate'] else 'СТОП'):>7}  {s['содержание']:7.3f}{s['спектр']:7.3f}{s['динамика']:7.3f}   {exp}")

checks = {
 "эталон ~0":            res["эталон сам с собой"]['ИТОГ'] < 0.2,
 "бездействие = 1.000":  abs(res["ничего не делать"]['ИТОГ']-1) < 1e-6,
 "громкость не влияет":  abs(res["тише в 3 раза"]['ИТОГ']-res["ничего не делать"]['ИТОГ']) < 1e-6,
 "починка в минус хуже": res["починил В МИНУС"]['ИТОГ'] > 1,
 "карикатура отсечена":  not res["карикатура"]['прошёл_gate'],
 "шум отсечён":          not res["шум вместо музыки"]['прошёл_gate'],
 "честная починка прошла gate": res["починил наклон"]['прошёл_gate'],
 "задержка 23 мс не штрафуется": abs(res["identity + 23 мс задержки"]['ИТОГ']-1) < 0.01,
 "задержка 93 мс не штрафуется": abs(res["identity + 93 мс задержки"]['ИТОГ']-1) < 0.01,
}
print()
for k,v in checks.items(): print(f"  {'OK  ' if v else 'ПРОВАЛ'} {k}")
print("\nсудья годен:", "ДА" if all(checks.values()) else "НЕТ")
