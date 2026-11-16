#!/usr/bin/env python3
"""Draw the synthetic demo pictures the screenshots show: mountain scenes, abstract patterns and portrait-shaped
figures, so no personal photos appear on the site.

Usage: make_demo_media.py HOME_DIR     writes HOME_DIR/Pictures/{Landscapes,Abstract,Portraits}/*.jpg
(run make_demo_videos.sh for the clips)
"""
import math
import os
import random
import sys

home = sys.argv[1]
root = os.path.join(home, "Pictures")
for d in ("Landscapes", "Abstract", "Portraits"):
    os.makedirs(os.path.join(root, d), exist_ok=True)
os.makedirs(os.path.join(home, "Documents"), exist_ok=True)
from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont
def lerp(a,b,t): return tuple(int(a[i]+(b[i]-a[i])*t) for i in range(3))
def grad(w,h,top,bot):
    im=Image.new("RGB",(w,h)); d=ImageDraw.Draw(im)
    for y in range(h): d.line([(0,y),(w,y)],fill=lerp(top,bot,y/h))
    return im
def landscape(name,top,bot,sun,layers,seed,w=1600,h=1067):
    r=random.Random(seed); im=grad(w,h,top,bot); d=ImageDraw.Draw(im)
    sx,sy=r.randint(w//4,3*w//4),int(h*0.45)
    glow=Image.new("RGB",(w,h),(0,0,0)); gd=ImageDraw.Draw(glow)
    gd.ellipse([sx-170,sy-170,sx+170,sy+170],fill=sun); glow=glow.filter(ImageFilter.GaussianBlur(80))
    im = ImageChops.add(im, glow)
    d=ImageDraw.Draw(im); d.ellipse([sx-60,sy-60,sx+60,sy+60],fill=sun)
    for i,col in enumerate(layers):
        base=h*(0.5+0.1*i); amp=h*(0.16-0.03*i)
        ph=[r.uniform(0,6.28) for _ in range(3)]; fr=[r.uniform(1.5,3)+i,r.uniform(4,7)+i,r.uniform(9,14)]
        pts=[(x, base-amp*(0.5+0.35*math.sin(fr[0]*x/w*3.14+ph[0])+0.2*math.sin(fr[1]*x/w*3.14+ph[1])+0.08*math.sin(fr[2]*x/w*3.14+ph[2]))) for x in range(0,w+8,8)]
        d.polygon(pts+[(w,h),(0,h)],fill=col)
    label(im,name); im.save(f"{root}/{name}.jpg",quality=90)
def label(im,name):
    d=ImageDraw.Draw(im)
    try: f=ImageFont.truetype("/usr/share/fonts/TTF/Inter-Regular.ttf",40)
    except Exception: f=ImageFont.load_default(40)
    d.text((im.width-30,im.height-24),name.split("/")[-1],font=f,fill=(255,255,255),anchor="rb")
def abstract(name,pal,seed,w=1600,h=1067,kind="waves"):
    r=random.Random(seed); im=grad(w,h,pal[0],pal[1]); d=ImageDraw.Draw(im,"RGBA")
    if kind=="waves":
        for i in range(14):
            col=lerp(pal[2],pal[3],i/13)+(150,); ph=r.uniform(0,6); pts=[(x,h*(0.15+i*0.06)+60*math.sin(x/120+ph+i*0.5)) for x in range(0,w+10,10)]
            d.polygon(pts+[(w,h),(0,h)],fill=col)
    elif kind=="circles":
        for i in range(40):
            x,y,rad=r.randint(0,w),r.randint(0,h),r.randint(40,260); col=lerp(pal[2],pal[3],r.random())+(r.randint(60,150),)
            d.ellipse([x-rad,y-rad,x+rad,y+rad],fill=col)
    else:
        n=16; cw=w//n
        for i in range(n):
            for j in range(h//cw+1): d.rectangle([i*cw,j*cw,i*cw+cw-6,j*cw+cw-6],fill=lerp(pal[2],pal[3],r.random())+(230,))
    label(im,name); im.save(f"{root}/{name}.jpg",quality=90)
def portrait(name,top,bot,skin,cloth,seed,w=1067,h=1600):
    im=grad(w,h,top,bot); d=ImageDraw.Draw(im)
    cx=w//2; d.ellipse([cx-330,h-720,cx+330,h+500],fill=cloth)
    d.rectangle([cx-80,h-900,cx+80,h-640],fill=skin); d.ellipse([cx-190,h-1250,cx+190,h-760],fill=skin)
    im=im.filter(ImageFilter.GaussianBlur(1.2)); label(im,name); im.save(f"{root}/{name}.jpg",quality=90)
landscape("Landscapes/dunes",(255,170,90),(255,226,160),(255,240,200),[(214,130,70),(176,96,56),(120,64,44)],1)
landscape("Landscapes/alpine",(70,110,190),(190,220,245),(255,255,240),[(96,120,160),(62,86,126),(34,52,86)],2)
landscape("Landscapes/coast",(250,140,120),(255,214,170),(255,235,190),[(120,90,140),(70,60,110),(30,34,70)],3)
landscape("Landscapes/forest",(120,170,150),(220,235,200),(250,250,220),[(70,120,90),(40,84,62),(20,50,40)],4)
landscape("Landscapes/aurora",(10,20,50),(40,120,120),(160,255,220),[(20,40,70),(12,24,48),(6,10,24)],5)
landscape("Landscapes/canyon",(230,110,70),(255,190,120),(255,225,170),[(190,80,50),(140,52,40),(90,34,32)],6)
abstract("Abstract/waves",((20,30,90),(60,20,120),(40,180,220),(230,80,180)),7,kind="waves")
abstract("Abstract/prism",((250,240,230),(240,220,240),(250,90,90),(70,90,250)),8,kind="circles")
abstract("Abstract/nebula",((10,5,30),(50,10,70),(255,120,200),(90,120,255)),9,kind="circles")
abstract("Abstract/mosaic",((20,20,30),(30,30,40),(255,180,60),(60,200,170)),10,kind="mosaic")
portrait("Portraits/studio-01",(60,70,110),(160,150,190),(236,190,160),(40,40,70),11)
portrait("Portraits/studio-02",(200,120,100),(250,210,170),(150,100,80),(30,80,90),12)
