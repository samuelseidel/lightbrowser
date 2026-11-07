function sorted(s){return s?s.split('; ').sort().join('; '):'(none)'}
async function echo(){return sorted((await (await fetch('/cookie/echo')).text()).replace(/^-$/,''))}
async function setHttp(v){await fetch('/cookie/set?v='+encodeURIComponent(v))}
