async function main(){var out=[];
 out.push('has='+await document.hasStorageAccess().then(String,function(e){return 'rejected '+err(e)}));
 out.push('hasUnpart='+(typeof document.hasUnpartitionedCookieAccess=='function'?await document.hasUnpartitionedCookieAccess().then(String,function(e){return 'rejected '+err(e)}):'n/a'));
 out.push('perm='+await navigator.permissions.query({name:'storage-access'}).then(function(s){return s.state},function(e){return 'rejected '+err(e)}));
 out.push('request='+await document.requestStorageAccess().then(function(v){return 'resolved '+v},function(e){return 'rejected '+err(e)}));
 out.push('has2='+await document.hasStorageAccess());
 log(out.join(' '))}
