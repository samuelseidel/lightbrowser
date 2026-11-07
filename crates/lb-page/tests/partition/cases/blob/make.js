function main(){var u=URL.createObjectURL(new Blob(["postMessage('aba blob')"],{type:'text/javascript'}));top.postMessage({blob:u},'*');log('made '+u.replace(/[0-9a-f-]{36}/,'UUID'))}
